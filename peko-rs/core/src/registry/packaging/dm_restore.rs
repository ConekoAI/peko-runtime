//! ADR-095 D4: a moved peko takes its DMs with it.
//!
//! The snapshot carries `local/channels.json` (each channel's id, kind,
//! participants, the peko's binding, and where remote members live) and
//! each DM's posts as `local/dms/<channel>.jsonl`. Import leaves the posts
//! in [`DM_LOGS_DIR`]; once the principal is loaded — so its actor key is
//! in the channel registry and pair ids come out right — [`restore_dms`]
//! recreates each DM at its pair id, records the other member's runtime,
//! merges the posts by `msg_id`, and removes the carried logs. Nothing is
//! invited and nothing wakes; the copy catches up with what it missed from
//! the other side on the next `tunnel_ready` (ADR-095 D3).

use std::path::Path;

use peko_channel::{ChannelEvent, ChannelId, ChannelPort, RemoteMember};
use peko_subject::{ActorRef, PrincipalId, Route};
use serde::Deserialize;
use tracing::{debug, warn};

/// Where import leaves a snapshot's DM posts, under the principal's local
/// root, until [`restore_dms`] replays them.
pub const DM_LOGS_DIR: &str = "dms.pending";

#[derive(Debug, Deserialize)]
struct ChannelEntry {
    channel: String,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    participants: Vec<Participant>,
    #[serde(default)]
    binding: Option<String>,
    #[serde(default)]
    remote: Vec<RemoteRow>,
}

#[derive(Debug, Deserialize)]
struct Participant {
    address: String,
    #[serde(default)]
    hub_id: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    identity_key: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RemoteRow {
    runtime_id: String,
    principal_id: String,
}

/// What [`restore_dms`] did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct DmRestore {
    /// DMs recreated or found, with the posts each one gained.
    pub restored: Vec<(ChannelId, usize)>,
    /// DMs left behind, with why (a peer only reachable on the source
    /// runtime, no binding, a key conflict).
    pub skipped: Vec<(String, String)>,
}

/// Replay the DMs a snapshot carried for `principal` (ADR-095 D4).
///
/// `local_root` is the principal's local root (holding `channels.json`
/// and [`DM_LOGS_DIR`]); `actor_key` its immutable key; `runtime` this
/// runtime's DID, so rows naming it are not recorded as remote. A
/// snapshot without carried DMs is a no-op.
///
/// # Errors
///
/// Only when `channels.json` exists but cannot be read; each DM's own
/// failure is reported in [`DmRestore::skipped`].
pub async fn restore_dms(
    port: &dyn ChannelPort,
    principal: &PrincipalId,
    actor_key: &str,
    local_root: &Path,
    runtime: &str,
) -> anyhow::Result<DmRestore> {
    let logs = local_root.join(DM_LOGS_DIR);
    let mut out = DmRestore::default();
    if !logs.is_dir() {
        return Ok(out);
    }
    let entries: Vec<ChannelEntry> = match tokio::fs::read(local_root.join("channels.json")).await {
        Ok(bytes) => serde_json::from_slice(&bytes)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e.into()),
    };
    let me = ActorRef::from(principal).canonical_address();
    for entry in entries.iter().filter(|e| e.kind.as_deref() == Some("dm")) {
        match restore_one(port, principal, actor_key, &me, entry, &logs, runtime).await {
            Ok(done) => out.restored.push(done),
            Err(e) => {
                debug!(channel = %entry.channel, "carried DM not restored: {e}");
                out.skipped.push((entry.channel.clone(), e.to_string()));
            }
        }
    }
    if let Err(e) = tokio::fs::remove_dir_all(&logs).await {
        warn!(dir = %logs.display(), "carried DM logs not removed: {e}");
    }
    Ok(out)
}

async fn restore_one(
    port: &dyn ChannelPort,
    principal: &PrincipalId,
    actor_key: &str,
    me: &str,
    entry: &ChannelEntry,
    logs: &Path,
    runtime: &str,
) -> anyhow::Result<(ChannelId, usize)> {
    let mine = entry
        .participants
        .iter()
        .find(|p| p.identity_key.as_deref() == Some(actor_key))
        .ok_or_else(|| anyhow::anyhow!("the peko is not in it"))?;
    let peer = entry
        .participants
        .iter()
        .find(|p| p.identity_key.as_deref() != Some(actor_key))
        .ok_or_else(|| anyhow::anyhow!("no other member"))?;
    let binding = entry
        .binding
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("the peko had no session bound to it"))?;
    let signing_did = entry
        .remote
        .iter()
        .find(|r| r.principal_id.starts_with("did:key:"))
        .map(|r| r.principal_id.as_str());
    let peer_ref = peer_here(port, peer, signing_did).await?;
    let channel = port.ensure_dm(principal, &peer_ref, binding).await?;

    let remote: Vec<RemoteMember> = entry
        .remote
        .iter()
        .filter(|r| r.runtime_id != runtime)
        .map(|r| RemoteMember {
            runtime_id: r.runtime_id.clone(),
            principal_id: r.principal_id.clone(),
        })
        .collect();
    let posts = carried_posts(
        &logs.join(format!("{}.jsonl", entry.channel)),
        &mine.address,
        me,
    )
    .await?;
    let added = port.restore_dm_copy(&channel, &remote, posts).await?;
    Ok((channel, added))
}

/// The DM's other member as this runtime names it. A person or peko with
/// a Hub ID is reached by it (and recorded with its key, as the directory
/// would); the source runtime's local user is this runtime's local user
/// (ADR-081 D6 step 2); a peko known only on the source runtime stays
/// behind (ADR-081 D1: dormant away from home).
async fn peer_here(
    port: &dyn ChannelPort,
    peer: &Participant,
    signing_did: Option<&str>,
) -> anyhow::Result<ActorRef> {
    let kind = peer.kind.as_deref().unwrap_or_default();
    match (kind, peer.hub_id.as_deref()) {
        ("principal", Some(hub)) => {
            let subject = ActorRef::Principal(Route::hub(hub.to_string()));
            port.register_actor(
                &subject,
                Some(hub),
                signing_did,
                peer.identity_key.as_deref(),
                None,
                peko_channel::Registration::Registered,
            )
            .await?;
            Ok(subject)
        }
        ("user", Some(hub)) => {
            let subject = ActorRef::User(Route::hub(hub.to_string()));
            // Known already (this runtime's own person, or a contact):
            // keep its record.
            if port.actor_info(&subject).await.identity_key == subject.canonical_address() {
                port.register_actor(
                    &subject,
                    Some(hub),
                    None,
                    None,
                    None,
                    peko_channel::Registration::Registered,
                )
                .await?;
            }
            Ok(subject)
        }
        ("user", None) => Ok(ActorRef::User("local".into())),
        _ => anyhow::bail!("{} is reachable only on the runtime it left", peer.address),
    }
}

/// The carried posts, with the peko's own posts re-authored to its address
/// here (a local-only peko's address names its old local ID).
async fn carried_posts(path: &Path, old_me: &str, me: &str) -> anyhow::Result<Vec<ChannelEvent>> {
    let bytes = match tokio::fs::read(path).await {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut posts = Vec::new();
    for line in bytes.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
        let Ok(mut event) = serde_json::from_slice::<ChannelEvent>(line) else {
            continue;
        };
        if let ChannelEvent::Posted { author, .. } = &mut event {
            if author == old_me {
                *author = me.to_string();
            }
            posts.push(event);
        }
    }
    Ok(posts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use peko_channel::{ChannelConfig, ChannelStore, Checkpoint, Registration};

    const HERE: &str = "did:key:zHere";
    const PEER_RUNTIME: &str = "did:key:zPeerRuntime";

    fn post(channel: &str, author: &str, id: &str, at: &str, text: &str) -> String {
        serde_json::to_string(&ChannelEvent::Posted {
            channel: ChannelId(channel.into()),
            author: author.into(),
            parent: None,
            text: text.into(),
            at: at.into(),
            via: None,
            msg_id: Some(id.into()),
            reply_to: None,
            attachments: Vec::new(),
            options: Vec::new(),
        })
        .unwrap()
    }

    /// ADR-095 D4: a carried DM with a registered peko comes back at its
    /// pair id, with the peer's runtime and every post; one with a peko
    /// known only on the source runtime stays behind; the logs go.
    #[tokio::test]
    async fn carried_dms_come_back_at_their_pair_ids() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ChannelStore::new(ChannelConfig {
            runtime_dir: tmp.path().join("runtime"),
            shared_dir: None,
        });
        // The moved peko, loaded here under a new local ID, same key.
        let me = PrincipalId("prin_here".into());
        store
            .register_actor(
                &ActorRef::from(&me),
                Some("mover-hub"),
                Some("did:key:zMover"),
                Some("mover-key"),
                Some("mover"),
                Registration::Registered,
            )
            .await
            .unwrap();
        let dm = peko_channel::dm_channel_id("mover-key", "ben-key");
        let stranded = peko_channel::dm_channel_id("mover-key", "local-peer-key");
        let local_root = tmp.path().join("local");
        let logs = local_root.join(DM_LOGS_DIR);
        std::fs::create_dir_all(&logs).unwrap();
        std::fs::write(
            local_root.join("channels.json"),
            serde_json::to_vec(&serde_json::json!([
                {
                    "channel": dm.as_str(),
                    "kind": "dm",
                    "binding": "/principal-ben",
                    "participants": [
                        {"address": "principal:local:prin_src", "kind": "principal",
                         "hub_id": "mover-hub", "identity_key": "mover-key"},
                        {"address": "principal:hub:ben-hub", "kind": "principal",
                         "hub_id": "ben-hub", "identity_key": "ben-key"},
                    ],
                    "remote": [
                        {"runtime_id": PEER_RUNTIME, "principal_id": "did:key:zBen"},
                        {"runtime_id": HERE, "principal_id": "did:key:zIgnored"},
                    ],
                },
                {
                    "channel": stranded.as_str(),
                    "kind": "dm",
                    "binding": "/principal-old",
                    "participants": [
                        {"address": "principal:local:prin_src", "kind": "principal",
                         "identity_key": "mover-key"},
                        {"address": "principal:local:prin_old", "kind": "principal",
                         "identity_key": "local-peer-key"},
                    ],
                },
                {"channel": "chan_group", "kind": "group"},
            ]))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(
            logs.join(format!("{}.jsonl", dm.as_str())),
            [
                post(
                    dm.as_str(),
                    "principal:local:prin_src",
                    "m1",
                    "2026-11-01T10:00:00Z",
                    "hi ben",
                ),
                post(
                    dm.as_str(),
                    "principal:hub:ben-hub",
                    "m2",
                    "2026-11-01T10:01:00Z",
                    "hi!",
                ),
            ]
            .join("\n"),
        )
        .unwrap();

        let done = restore_dms(&store, &me, "mover-key", &local_root, HERE)
            .await
            .unwrap();
        assert_eq!(done.restored, vec![(dm.clone(), 2)]);
        assert_eq!(done.skipped.len(), 1);
        assert_eq!(done.skipped[0].0, stranded.as_str());
        assert!(!logs.exists(), "carried logs removed");

        let posts: Vec<(String, String)> = store
            .peek(&dm, &Checkpoint::zero())
            .await
            .unwrap()
            .into_iter()
            .filter_map(|ev| match ev {
                ChannelEvent::Posted { author, text, .. } => Some((author, text)),
                _ => None,
            })
            .collect();
        assert_eq!(posts[0].1, "hi ben");
        assert_ne!(posts[0].0, "principal:local:prin_src", "re-authored");
        assert_eq!(posts[1].1, "hi!");
        assert_eq!(
            store.list_remote_members(&dm).await.unwrap(),
            vec![RemoteMember {
                runtime_id: PEER_RUNTIME.into(),
                principal_id: "did:key:zBen".into(),
            }]
        );
        assert_eq!(
            store
                .binding_for(&dm, &me)
                .await
                .unwrap()
                .map(|b| b.session)
                .as_deref(),
            Some("/principal-ben")
        );
        // It now counts as a peko-to-peko DM, so it catches up (D3).
        assert_eq!(store.peko_dms().await.unwrap(), vec![dm.clone()]);

        // Restoring again with nothing carried is a no-op.
        assert_eq!(
            restore_dms(&store, &me, "mover-key", &local_root, HERE)
                .await
                .unwrap(),
            DmRestore::default()
        );
    }
}
