//! `persona` domain request handler.
//!
//! Owns the `RequestPacket::PersonaDraft` IPC variant. The handler
//! is a thin pass-through to a configured model via
//! [`peko_providers::core::Provider::chat_with_system`] — there is no
//! session, no memory injection, no streaming, no hooks. This is the
//! "draft a persona from a one-sentence description" path surfaced
//! by `peko principal persona set <name> --from "…"`. The CLI parses
//! the LLM JSON and writes `principal.toml` + `agents/primary.md`.
//!
//! Boundary rules (F6):
//! - Dependency inversion: the consumer (`ipc::handlers::persona`)
//!   defines the [`PersonaHost`] trait; the producer
//!   (`daemon::state`) implements it.
//! - F6: this module must not import any other `ipc::handlers::*` module.

use std::sync::Arc;

use async_trait::async_trait;

use crate::ipc::handlers::RequestHandler;
use crate::ipc::packet::{RequestPacket, ResponsePacket};
use crate::ipc::response_sink::ResponseSink;
use crate::ipc::send_response::send_response;
use crate::ipc::server::PeerAddr;
use peko_auth::caller::CallerContext;

/// Narrow port the `persona` handler uses to call the LLM. The
/// daemon-side impl owns the `LlmResolver` and `Provider`
/// construction; the handler hands off the system prompt + user
/// sentence and gets back the raw model output. Returning the raw
/// text (rather than `Arc<Provider>`) sidesteps the
/// `async_trait`-vs-Arc lifetime quirk and keeps the trait surface
/// minimal.
#[async_trait]
pub(crate) trait PersonaHost: Send + Sync {
    /// Call the configured model with the persona-draft system prompt
    /// and the user's one-sentence `from`. Returns the LLM's raw
    /// text. The handler parses JSON from it.
    ///
    /// Arguments are owned `String` so the trait plays nicely with
    /// `async_trait`'s lifetime expansion; the handler already has
    /// the values in hand and can `.clone()` cheaply.
    async fn draft_persona(
        &self,
        model_id: String,
        system: String,
        from: String,
    ) -> anyhow::Result<String>;
}

/// `persona` domain request handler. Constructed with an
/// `Arc<dyn PersonaHost>` (typically `Arc::new(app_state.clone())`
/// from the dispatcher).
pub(crate) struct PersonaHandler {
    host: Arc<dyn PersonaHost>,
}

impl PersonaHandler {
    pub(crate) fn new(host: Arc<dyn PersonaHost>) -> Self {
        Self { host }
    }
}

/// System prompt that instructs the LLM to emit a single JSON object
/// matching the persona schema below. Kept in this module (not in the
/// daemon's config) because the schema is part of the IPC contract —
/// changing it requires a coordinated CLI parser update.
///
/// The hardcoded `{{memory}}` line in the primary_md_body is a
/// placeholder recognized by `peko_engine::prompt::placeholder`;
/// `PromptRenderer` substitutes it with the principal's MEMORY.md
/// at render time. The renderer is tolerant of missing placeholders
/// (see `replace_placeholders(.., remove_missing=true)`), but
/// `{{memory}}` is what a non-technical user expects to see.
const DRAFT_SYSTEM_PROMPT: &str = r#"You are a persona designer for an AI assistant called Peko.

The user gives you ONE sentence describing who/what they want the
assistant to be. Reply with ONLY a single JSON object (no prose, no
markdown fences, no commentary before or after). The JSON must match
this schema:

{
  "display_name": "<short title-case human name, e.g. \"Rust Reviewer\">",
  "description": "<one paragraph: who the assistant is, what it does, who it serves>",
  "goals": ["<3-5 concrete goals the assistant should optimize for>"],
  "values": ["<3-5 guiding principles (e.g. cite sources, be concise, never invent)>"],
  "style": "<tone, length, formatting preferences — e.g. \"Concise. Cite doc.rust-lang.org. Bullet points preferred.\">",
  "primary_md_body": "<Markdown body for the agent prompt. Must include the literal placeholder {{memory}} on its own line.>"
}

Rules:
- Output ONLY the JSON object. No preamble, no explanation, no closing remarks.
- Keep goals and values to 3-5 items each — too many dilutes the persona.
- primary_md_body should be Markdown with a brief persona intro and the {{memory}} marker.
- Use the user's sentence as the seed; do not invent facts about a domain the user did not name.
"#;

#[async_trait]
impl RequestHandler for PersonaHandler {
    fn domain(&self) -> &'static str {
        "persona"
    }

    fn matches(&self, request: &RequestPacket) -> bool {
        matches!(request, RequestPacket::PersonaDraft { .. })
    }

    async fn handle(
        &self,
        request: RequestPacket,
        _caller: &CallerContext,
        sink: &dyn ResponseSink,
        _peer: &PeerAddr,
    ) -> anyhow::Result<()> {
        let RequestPacket::PersonaDraft {
            request_id,
            model_id,
            from,
        } = request
        else {
            unreachable!("PersonaHandler::matches allowed an unhandled variant");
        };

        if model_id.is_empty() {
            let response = ResponsePacket::Error {
                request_id,
                message: "model_id must not be empty".to_string(),
            };
            send_response(sink, response).await?;
            return Ok(());
        }
        if from.trim().is_empty() {
            let response = ResponsePacket::Error {
                request_id,
                message: "from description must not be empty".to_string(),
            };
            send_response(sink, response).await?;
            return Ok(());
        }

        // Hand off to the host. The daemon owns the resolver +
        // vault; the handler must not reach either directly (F6
        // boundary). `temperature = 0.7` is hardcoded in the host
        // impl; it gives the LLM room to draft while staying close
        // to the user's intent.
        let raw = match self
            .host
            .draft_persona(model_id.clone(), DRAFT_SYSTEM_PROMPT.to_string(), from.clone())
            .await
        {
            Ok(s) => s,
            Err(e) => {
                let response = ResponsePacket::Error {
                    request_id,
                    message: format!("persona draft LLM call failed: {e:#}"),
                };
                send_response(sink, response).await?;
                return Ok(());
            }
        };

        // Try to parse the LLM output as the schema's JSON. If the
        // model returned non-JSON (truncation, model laziness), the
        // CLI falls back to rendering the raw text in its preview;
        // either way the LLM's bytes reach the caller.
        let parse_ok = serde_json::from_str::<serde_json::Value>(&raw).is_ok();
        let response = ResponsePacket::PersonaDrafted {
            request_id,
            content: raw,
            parse_ok,
        };
        send_response(sink, response).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    //! Pin the wire shape for persona_draft so a future change to the
    //! system prompt or the response envelope surfaces as a test
    //! failure rather than as the CLI's `persona set` silently
    //! emitting unparseable JSON.

    use super::*;
    use crate::ipc::packet::{RequestPacket, ResponsePacket};

    #[test]
    fn persona_handler_matches_only_draft_variant() {
        let host = Arc::new(StubHost::default());
        let handler = PersonaHandler::new(host);
        assert!(handler.matches(&RequestPacket::PersonaDraft {
            request_id: 1,
            model_id: "x".into(),
            from: "y".into(),
        }));
        assert!(!handler.matches(&RequestPacket::ModelTest {
            request_id: 1,
            id: "x".into(),
        }));
        assert!(!handler.matches(&RequestPacket::PrincipalCreate {
            request_id: 1,
            name: "x".into(),
            description: None,
            model_id: "x".into(),
        }));
    }

    #[test]
    fn drafted_envelope_round_trip_with_parse_ok() {
        let resp = ResponsePacket::PersonaDrafted {
            request_id: 7,
            content: r#"{"display_name":"Rust Reviewer"}"#.to_string(),
            parse_ok: true,
        };
        let bytes = resp.to_bytes().unwrap();
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::PersonaDrafted {
                request_id,
                parse_ok,
                content,
            } => {
                assert_eq!(request_id, 7);
                assert!(parse_ok);
                assert_eq!(content, r#"{"display_name":"Rust Reviewer"}"#);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn drafted_envelope_can_carry_parse_failure_for_fallback() {
        // If the model returned non-JSON, parse_ok must serialize as
        // false so the CLI can choose between structured parsing
        // and a prose preview.
        let resp = ResponsePacket::PersonaDrafted {
            request_id: 8,
            content: "Sure! Here's a persona for you: ...".to_string(),
            parse_ok: false,
        };
        let bytes = resp.to_bytes().unwrap();
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::PersonaDrafted { parse_ok, .. } => assert!(!parse_ok),
            _ => panic!("Wrong variant"),
        }
    }

    /// Stub host used only to satisfy the trait bounds in
    /// `PersonaHandler::new` for the `matches` test. `draft_persona`
    /// is never called in that test.
    #[derive(Default)]
    struct StubHost;
    #[async_trait]
    impl PersonaHost for StubHost {
        async fn draft_persona(
            &self,
            _model_id: String,
            _system: String,
            _from: String,
        ) -> anyhow::Result<String> {
            Err(anyhow::anyhow!("stub"))
        }
    }
}