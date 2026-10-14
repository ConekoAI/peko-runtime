//! Redaction and size caps for ledger params and result excerpts
//! (ADR-079 D3).
//!
//! Three rules, strongest first:
//!
//! 1. Secrets the runtime itself holds (vault credentials, live run
//!    tokens) are replaced by `[secret:<label>]` wherever they appear.
//! 2. Values under sensitive keys (`*key`, `*token`, `*secret`,
//!    `password`, `authorization`, `cookie`) become `[redacted]`.
//! 3. Common credential shapes in free text (bearer headers, `sk-…`,
//!    `ghp_…`, `xox?-…`, AWS access keys, PEM private keys) become
//!    `[redacted]`.
//!
//! Redaction is best-effort: a secret the runtime does not know, in a
//! shape it does not recognize, is stored as written.

use std::sync::{Arc, LazyLock, Mutex, RwLock};
use std::time::{Duration, Instant};

use regex::Regex;
use serde_json::{Map, Value};

/// A string leaf in recorded params is cut to this many bytes.
pub const PARAM_LEAF_CAP: usize = 2 * 1024;
/// Recorded params as a whole are cut to this many bytes.
pub const PARAMS_CAP: usize = 8 * 1024;
/// A result excerpt is cut to this many bytes.
pub const EXCERPT_CAP: usize = 4 * 1024;

/// Secrets shorter than this are not matched exactly: they would hit
/// ordinary text.
const MIN_SECRET_LEN: usize = 8;

/// How long a snapshot of the known secrets is reused.
const SECRETS_TTL: Duration = Duration::from_secs(1);

/// Where the runtime's own secrets come from, as `(label, value)`.
pub trait SecretSource: Send + Sync {
    fn secrets(&self) -> Vec<(String, String)>;
}

/// The redactor the dispatcher applies to every ledger record.
#[derive(Default)]
pub struct Redactor {
    source: RwLock<Option<Arc<dyn SecretSource>>>,
    cache: Mutex<Option<(Instant, Arc<Vec<(String, String)>>)>>,
}

impl std::fmt::Debug for Redactor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Redactor").finish_non_exhaustive()
    }
}

impl Redactor {
    /// Bind where known secrets come from.
    pub fn set_source(&self, source: Arc<dyn SecretSource>) {
        *self.source.write().expect("redactor source poisoned") = Some(source);
        *self.cache.lock().expect("redactor cache poisoned") = None;
    }

    fn known(&self) -> Arc<Vec<(String, String)>> {
        let mut cache = self.cache.lock().expect("redactor cache poisoned");
        if let Some((at, secrets)) = cache.as_ref() {
            if at.elapsed() < SECRETS_TTL {
                return Arc::clone(secrets);
            }
        }
        let mut secrets: Vec<(String, String)> = self
            .source
            .read()
            .expect("redactor source poisoned")
            .as_ref()
            .map(|source| source.secrets())
            .unwrap_or_default()
            .into_iter()
            .filter(|(_, value)| value.len() >= MIN_SECRET_LEN)
            .collect();
        // Longest first, so a secret containing another is replaced whole.
        secrets.sort_by(|a, b| b.1.len().cmp(&a.1.len()));
        let secrets = Arc::new(secrets);
        *cache = Some((Instant::now(), Arc::clone(&secrets)));
        secrets
    }

    /// `params` redacted and capped for the ledger.
    #[must_use]
    pub fn params(&self, params: &Value) -> Value {
        let known = self.known();
        let redacted = redact_value(params, &known, None);
        let serialized = redacted.to_string();
        if serialized.len() <= PARAMS_CAP {
            return redacted;
        }
        serde_json::json!({
            "_truncated": true,
            "bytes": serialized.len(),
            "preview": head(&serialized, PARAMS_CAP),
        })
    }

    /// `text` redacted, for a result excerpt (capping is the caller's).
    #[must_use]
    pub fn text(&self, text: &str) -> String {
        redact_text(text, &self.known())
    }
}

fn redact_value(value: &Value, known: &[(String, String)], key: Option<&str>) -> Value {
    if key.is_some_and(is_sensitive_key) && !value.is_null() {
        return Value::String("[redacted]".into());
    }
    match value {
        Value::String(text) => Value::String(head(&redact_text(text, known), PARAM_LEAF_CAP)),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| redact_value(item, known, None))
                .collect(),
        ),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), redact_value(v, known, Some(k))))
                .collect::<Map<_, _>>(),
        ),
        other => other.clone(),
    }
}

/// Keys whose values are credentials by name.
fn is_sensitive_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase().replace('-', "_");
    // `max_tokens`, `token_budget`: counts, not credentials.
    if key.contains("tokens") || key.ends_with("_count") {
        return false;
    }
    key.ends_with("key")
        || key.ends_with("token")
        || key.ends_with("secret")
        || key.contains("password")
        || key.contains("passphrase")
        || key == "authorization"
        || key == "cookie"
}

static PATTERNS: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    [
        (
            r"-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z ]*PRIVATE KEY-----",
            "[redacted private key]",
        ),
        (r"(?i)\b(bearer|basic)\s+[A-Za-z0-9._~+/=-]{8,}", "$1 [redacted]"),
        (
            r#"(?i)\b(authorization|x-api-key|api[-_]?key|access[-_]?token|auth[-_]?token|secret|password|passwd)(["']?\s*[:=]\s*["']?)([^\s"'&,;]{4,})"#,
            "$1$2[redacted]",
        ),
        (r"\bsk-[A-Za-z0-9_-]{16,}", "[redacted]"),
        (r"\bgh[pousr]_[A-Za-z0-9]{20,}", "[redacted]"),
        (r"\bxox[abprs]-[A-Za-z0-9-]{10,}", "[redacted]"),
        (r"\bAKIA[0-9A-Z]{16}\b", "[redacted]"),
    ]
    .into_iter()
    .map(|(pattern, replacement)| {
        (
            Regex::new(pattern).expect("ledger redaction pattern compiles"),
            replacement,
        )
    })
    .collect()
});

fn redact_text(text: &str, known: &[(String, String)]) -> String {
    // Known secrets first, as sentinels the patterns cannot match, so a
    // pattern never re-redacts (or splits) a `[secret:<label>]` marker.
    let mut out = text.to_string();
    let mut found = Vec::new();
    for (label, secret) in known {
        if out.contains(secret.as_str()) {
            out = out.replace(secret.as_str(), &format!("\u{0}{}\u{0}", found.len()));
            found.push(label);
        }
    }
    for (pattern, replacement) in PATTERNS.iter() {
        if pattern.is_match(&out) {
            out = pattern.replace_all(&out, *replacement).into_owned();
        }
    }
    for (index, label) in found.iter().enumerate() {
        out = out.replace(&format!("\u{0}{index}\u{0}"), &format!("[secret:{label}]"));
    }
    out
}

/// The first `cap` bytes of `text`, on a char boundary, marked when cut.
#[must_use]
pub fn head(text: &str, cap: usize) -> String {
    if text.len() <= cap {
        return text.to_string();
    }
    let mut end = cap;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…(truncated)", &text[..end])
}

/// The last `cap` bytes of `text`, on a char boundary, marked when cut.
#[must_use]
pub fn tail(text: &str, cap: usize) -> String {
    if text.len() <= cap {
        return text.to_string();
    }
    let mut start = text.len() - cap;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    format!("(truncated)…{}", &text[start..])
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct Fixed(Vec<(String, String)>);
    impl SecretSource for Fixed {
        fn secrets(&self) -> Vec<(String, String)> {
            self.0.clone()
        }
    }

    fn redactor(secrets: &[(&str, &str)]) -> Redactor {
        let redactor = Redactor::default();
        redactor.set_source(Arc::new(Fixed(
            secrets
                .iter()
                .map(|(l, v)| ((*l).to_string(), (*v).to_string()))
                .collect(),
        )));
        redactor
    }

    #[test]
    fn known_secrets_are_replaced_by_label_anywhere() {
        let r = redactor(&[("vault:anthropic", "zz-very-secret-value-123")]);
        let params = r.params(&json!({
            "command": "curl -d 'k=zz-very-secret-value-123' https://x"
        }));
        assert_eq!(
            params["command"],
            "curl -d 'k=[secret:vault:anthropic]' https://x"
        );
        assert_eq!(
            r.text("out: zz-very-secret-value-123"),
            "out: [secret:vault:anthropic]"
        );
    }

    #[test]
    fn short_known_values_are_not_matched() {
        let r = redactor(&[("tiny", "abc")]);
        assert_eq!(r.text("abcdef"), "abcdef");
    }

    #[test]
    fn sensitive_keys_are_redacted_but_counts_are_not() {
        let r = redactor(&[]);
        let params = r.params(&json!({
            "api_key": "anything",
            "headers": { "Authorization": "x", "Accept": "y" },
            "max_tokens": 100,
            "password": null,
        }));
        assert_eq!(params["api_key"], "[redacted]");
        assert_eq!(params["headers"]["Authorization"], "[redacted]");
        assert_eq!(params["headers"]["Accept"], "y");
        assert_eq!(params["max_tokens"], 100);
        assert!(params["password"].is_null());
    }

    #[test]
    fn credential_shapes_in_free_text_are_redacted() {
        let r = redactor(&[]);
        let text = r.text(
            "curl -H 'Authorization: Bearer abcdefghijklmnop' -H 'x-api-key: qwertyuiop' \
             sk-ant-abcdefghijklmnopqrstu ghp_abcdefghijklmnopqrstuvwxyz0123 \
             password=hunter22 AKIAABCDEFGHIJKLMNOP",
        );
        for leaked in [
            "abcdefghijklmnop",
            "qwertyuiop",
            "sk-ant-",
            "ghp_",
            "hunter22",
            "AKIAABCDEFGHIJKLMNOP",
        ] {
            assert!(!text.contains(leaked), "{leaked} leaked: {text}");
        }
        let pem = "-----BEGIN OPENSSH PRIVATE KEY-----\nAAAA\n-----END OPENSSH PRIVATE KEY-----";
        assert_eq!(r.text(pem), "[redacted private key]");
    }

    #[test]
    fn caps_hold() {
        let r = redactor(&[]);
        let leaf = r.params(&json!({ "content": "x".repeat(10_000) }));
        assert!(leaf["content"].as_str().unwrap().len() < PARAM_LEAF_CAP + 20);
        let many: Map<String, Value> = (0..20)
            .map(|i| (format!("k{i}"), Value::String("y".repeat(1_500))))
            .collect();
        let whole = r.params(&Value::Object(many));
        assert_eq!(whole["_truncated"], true);
        assert!(whole["preview"].as_str().unwrap().len() < PARAMS_CAP + 20);
        assert_eq!(tail("abcdef", 3), "(truncated)…def");
        assert_eq!(head("héllo", 2), "h…(truncated)");
    }
}
