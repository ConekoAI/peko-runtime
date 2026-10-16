//! Standing conversation sessions, keyed exclusively by their channel binding.
//! Display slugs and titles organize private working memory; channel membership
//! identifies interlocutors. Every creation is serialized by the session store lock.

use std::sync::Arc;

use anyhow::Result;
use peko_auth::ActorRef;
use peko_session::manager::SessionManager;
use peko_session::path::MAX_SLUG_LEN;
use peko_session::{SessionCreateOptions, SessionMetadata};
use tokio::sync::RwLock;

use crate::principal::routers::root::trunk_session_id;

/// Upper bound on `-N` suffix attempts when a peer's derived slug
/// collides with a different session under the trunk (sanitized
/// collisions like `user:Foo Bar` vs `user:foo-bar`). Ten spare slots
/// per peer is far past any realistic collision chain.
const MAX_SLUG_ATTEMPTS: usize = 10;

/// Collision-resistant slug fragment for a principal DID.
fn principal_slug_fragment(did: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(did.as_bytes()))[..16].to_string()
}

/// The stable per-parent slug for a peer's standing child of the
/// trunk.
///
/// - `user:local` → `local-user` (the CLI owner's child);
/// - `user:{id}` → `user-{sanitized}` where sanitization keeps
///   lowercase ascii alphanumerics, maps everything else to `-`,
///   collapses repeats, and trims leading/trailing `-`;
/// - `visitor:{id}` → `visitor-{sanitized}` (ADR-058 D5 — hub-minted
///   anonymous peers; same sanitization as users, distinct slug
///   space);
/// - `principal:{did}` → `principal-{fragment}` where the fragment is
///   the first 16 hex digits of the DID's SHA-256 (DIDs share long
///   scheme prefixes such as `did:peko:local:`, so a prefix of the DID
///   itself would collide).
///
/// The result is capped at [`MAX_SLUG_LEN`] chars and validated
/// against `peko_session::path::validate_slug`. `ActorRef::Public` is
/// not a session peer (ADR-039) and is a structured error.
pub fn peer_child_slug(peer: &ActorRef) -> Result<String> {
    let slug = match peer {
        ActorRef::User(id) if id == "local" => "local-user".to_string(),
        ActorRef::User(id) => {
            let sanitized = sanitize_slug_segment(&id.to_string());
            let slug = if sanitized.is_empty() {
                "user".to_string()
            } else {
                format!("user-{sanitized}")
            };
            cap_slug(&slug)
        }
        ActorRef::Visitor(id) => {
            // ADR-058 D5: visitors are ordinary non-owner peers with
            // their own `/visitor-<id>` slug space. The typed-claim
            // gate (`ActorRef::from_bridge_claim`) already rejects ids
            // whose raw claims contain `:` and the reserved `local`, so a visitor
            // slug can never collide with `/local-user` or smuggle a
            // `principal:` reference.
            let sanitized = sanitize_slug_segment(&id.to_string());
            let slug = if sanitized.is_empty() {
                "visitor".to_string()
            } else {
                format!("visitor-{sanitized}")
            };
            cap_slug(&slug)
        }
        ActorRef::Principal(did) => format!("principal-{}", principal_slug_fragment(&did.as_str())),
        ActorRef::Public => {
            anyhow::bail!("public access is not a session peer — no peer child can be provisioned")
        }
    };
    peko_session::path::validate_slug(&slug)?;
    Ok(slug)
}

/// Locate the persisted conversation binding using its immutable actor pair.
pub async fn find_dm_child(
    metas: &[SessionMetadata],
    principal: &peko_subject::PrincipalId,
    peer: &ActorRef,
    port: &Arc<dyn peko_channel::ChannelPort>,
) -> Option<String> {
    let me = port.actor_info(&ActorRef::from(principal)).await;
    let other = port.actor_info(peer).await;
    let channel = peko_channel::dm_channel_id(&me.identity_key, &other.identity_key);
    metas
        .iter()
        .find(|m| m.channel_id.as_deref() == Some(channel.as_str()))
        .map(|m| m.session_id.to_string())
}
#[cfg(test)]
pub fn find_peer_child(metas: &[SessionMetadata], peer: &ActorRef) -> Option<String> {
    metas
        .iter()
        .find(|m| m.channel_id.is_some() && m.title.as_deref() == Some(peer.to_string().as_str()))
        .map(|m| m.session_id.to_string())
}
pub(crate) fn group_child_base_slug(channel: &str) -> String {
    cap_slug(&format!(
        "group-{}",
        sanitize_slug_segment(channel.strip_prefix("group:").unwrap_or(channel))
    ))
}
pub async fn ensure_group_child(
    agent_name: &str,
    channel: &str,
    session_manager: &Arc<RwLock<SessionManager>>,
) -> Result<String> {
    ensure_channel_child(
        agent_name,
        channel,
        &group_child_base_slug(channel),
        channel,
        session_manager,
    )
    .await
}
/// The principal's group session for `channel`, find-or-created inside
/// the channel's compartment, with its binding recorded (ADR-082 D2).
/// The session side is authoritative: a missing or stale binding is
/// rewritten to name it.
pub async fn ensure_group_binding(
    port: &Arc<dyn peko_channel::ChannelPort>,
    principal: &peko_subject::PrincipalId,
    agent_name: &str,
    channel: &peko_channel::ChannelId,
    session_manager: &Arc<RwLock<SessionManager>>,
) -> Result<String> {
    let session = ensure_group_child(agent_name, channel.as_str(), session_manager).await?;
    if port
        .binding_for(channel, principal)
        .await?
        .is_none_or(|b| b.session != session)
    {
        port.bind(channel, principal, &session).await?;
    }
    Ok(session)
}
pub async fn ensure_channel_child(
    agent_name: &str,
    channel: &str,
    base_slug: &str,
    title: &str,
    session_manager: &Arc<RwLock<SessionManager>>,
) -> Result<String> {
    let mut manager = session_manager.write().await;
    let metas = manager.list_all_sessions(false).await?;
    if let Some(meta) = metas
        .iter()
        .find(|m| m.channel_id.as_deref() == Some(channel))
    {
        return Ok(meta.session_id.to_string());
    }
    let options = SessionCreateOptions::new()
        .with_parent(trunk_session_id())
        .with_trigger("spawn")
        .with_title(title);
    let session = manager
        .create_session(agent_name, &ActorRef::Public, options)
        .await?;
    let id = session.session_id().to_string();
    for attempt in 0..MAX_SLUG_ATTEMPTS {
        let slug = if attempt == 0 {
            base_slug.to_string()
        } else {
            format!("{base_slug}-{}", attempt + 1)
        };
        match manager.set_session_slug(&id, Some(slug)).await {
            Ok(()) => break,
            Err(e) if e.to_string().contains("unique per parent") => continue,
            Err(e) => return Err(e),
        }
    }
    manager.bind_session_channel(&id, channel).await?;
    // ADR-078 D3: a conversation's session is its own compartment.
    manager
        .set_session_compartment(
            &id,
            peko_tools_core::Compartment::Conversation {
                channel: channel.to_string(),
            },
        )
        .await?;
    Ok(id)
}
/// Sanitize a user id into a slug segment: lowercase ascii
/// alphanumerics kept, everything else mapped to `-`, repeats
/// collapsed, leading/trailing `-` trimmed.
fn sanitize_slug_segment(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut last_dash = false;
    for c in raw.chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() {
            out.push(c);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

/// Truncate to the slug length cap (char-safe).
fn cap_slug(slug: &str) -> String {
    slug.chars().take(MAX_SLUG_LEN).collect()
}

#[cfg(test)]
pub async fn ensure_peer_child(
    agent_name: &str,
    peer: &ActorRef,
    sessions: &Arc<RwLock<SessionManager>>,
) -> Result<String> {
    let channel = peko_channel::dm_channel_id("fixture", &peer.to_string());
    ensure_channel_child(
        agent_name,
        channel.as_str(),
        &peer_child_slug(peer)?,
        &peer.to_string(),
        sessions,
    )
    .await
}
