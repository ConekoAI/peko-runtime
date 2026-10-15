//! A principal's own contact book (ADR-081 D5).
//!
//! Learned contacts are the principal's knowledge, so they live in its
//! Local tier (`local/contacts.json`) and travel with it in a `.peko`
//! snapshot. Each record carries the actor's immutable key and its routes;
//! a local route names the runtime that issued it. The runtime's actor
//! registry (`runtime/actors.json`) stays the routing index: a contact is
//! *reachable* when the registry here knows it under the same key, and
//! *dormant* otherwise — kept and listed, but never addressable.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use peko_channel::{ActorInfo, ChannelPort};
use peko_subject::Subject;
use serde::{Deserialize, Serialize};

/// The file name inside the principal's Local root.
pub const CONTACTS_FILE: &str = "contacts.json";
const LOCK_TIMEOUT_MS: u64 = 5_000;

/// A local route and the runtime that issued it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalRoute {
    pub runtime: String,
    pub id: String,
}

/// One learned contact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContactRecord {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local: Option<LocalRoute>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hub_id: Option<String>,
    pub identity_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signing_did: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub registration: String,
}

impl ContactRecord {
    /// A record of a registry actor as known on `runtime`.
    #[must_use]
    pub fn from_actor(actor: &ActorInfo, runtime: &str) -> Self {
        Self {
            kind: actor.kind.clone(),
            local: actor.local_id.clone().map(|id| LocalRoute {
                runtime: runtime.to_string(),
                id,
            }),
            hub_id: actor.hub_id.clone(),
            identity_key: actor.identity_key.clone(),
            signing_did: None,
            name: actor.name.clone(),
            registration: actor.registration.clone(),
        }
    }

    /// The reference the record names on `runtime`: a Hub route when it
    /// has one, else its local route — qualified when it belongs to
    /// another runtime, so it can never match a local actor here.
    #[must_use]
    pub fn subject_on(&self, runtime: &str) -> Option<Subject> {
        let text = match (&self.hub_id, &self.local) {
            (Some(hub), _) => format!("{}:hub:{hub}", self.kind),
            (None, Some(local)) if local.runtime == runtime => {
                format!("{}:local:{}", self.kind, local.id)
            }
            (None, Some(local)) => format!("{}:local:{}@{}", self.kind, local.id, local.runtime),
            (None, None) => return None,
        };
        text.parse().ok()
    }

    /// Whether the registry on `runtime` routes this record to the same
    /// immutable actor.
    #[must_use]
    pub fn reachable_on(&self, port: &dyn ChannelPort, runtime: &str) -> bool {
        let routes = [
            self.local
                .as_ref()
                .filter(|local| local.runtime == runtime)
                .and_then(|local| format!("{}:local:{}", self.kind, local.id).parse().ok()),
            self.hub_id
                .as_ref()
                .and_then(|hub| format!("{}:hub:{hub}", self.kind).parse().ok()),
        ];
        routes
            .into_iter()
            .flatten()
            .any(|route: Subject| port.key_of(&route).0 == self.identity_key)
    }
}

/// The principal's contact book file.
#[derive(Debug, Clone)]
pub struct ContactBook {
    path: PathBuf,
}

impl ContactBook {
    /// The book in a principal's Local root.
    #[must_use]
    pub fn in_local_root(local_root: &Path) -> Self {
        Self {
            path: local_root.join(CONTACTS_FILE),
        }
    }

    /// The book of a loaded principal.
    #[must_use]
    pub fn of(principal: &crate::principal::Principal) -> Self {
        let sessions = principal.memory.sessions_dir();
        Self::in_local_root(sessions.parent().unwrap_or(&sessions))
    }

    /// Every learned contact.
    pub fn list(&self) -> Result<Vec<ContactRecord>> {
        match std::fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .with_context(|| format!("decode {}", self.path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(e.into()),
        }
    }

    /// Learn (or refresh) `record`, keyed by its immutable key. Concurrent
    /// learners serialize on a file lock; the write is atomic.
    pub async fn remember(&self, record: ContactRecord) -> Result<()> {
        self.update(|records| {
            match records
                .iter_mut()
                .find(|r| r.identity_key == record.identity_key)
            {
                Some(existing) => *existing = record,
                None => records.push(record),
            }
        })
        .await
    }

    /// Rewrite every record under the lock.
    pub async fn update(&self, change: impl FnOnce(&mut Vec<ContactRecord>)) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let _lock = peko_fs_persistence::FileLock::acquire(&self.path, LOCK_TIMEOUT_MS).await?;
        let mut records = self.list()?;
        change(&mut records);
        records.sort_by(|a, b| a.identity_key.cmp(&b.identity_key));
        let temp = self
            .path
            .with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
        tokio::fs::write(&temp, serde_json::to_vec_pretty(&records)?).await?;
        tokio::fs::rename(&temp, &self.path).await?;
        Ok(())
    }
}

/// Move the retired per-runtime `known_by` lists out of `actors.json` into
/// each hosted principal's contact book (ADR-081 D5). Records stay in the
/// registry as routing entries; only the relationship moves.
pub async fn migrate_known_by(
    runtime_dir: &Path,
    runtime: &str,
    principals: &[std::sync::Arc<crate::principal::Principal>],
) -> Result<()> {
    let path = runtime_dir.join("actors.json");
    let Ok(bytes) = std::fs::read(&path) else {
        return Ok(());
    };
    let records: Vec<serde_json::Value> = serde_json::from_slice(&bytes)?;
    for principal in principals {
        let book = ContactBook::of(principal);
        for record in &records {
            let learned = record
                .get("known_by")
                .and_then(|v| v.as_array())
                .is_some_and(|ids| ids.iter().any(|id| id.as_str() == Some(&principal.id.0)));
            if !learned {
                continue;
            }
            let Ok(mut info) = serde_json::from_value::<ActorInfo>(record["info"].clone()) else {
                continue;
            };
            info.identity_key = record["identity_key"].as_str().unwrap_or_default().into();
            let mut contact = ContactRecord::from_actor(&info, runtime);
            contact.signing_did = record["signing_did"].as_str().map(str::to_owned);
            book.remember(contact).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(local: Option<(&str, &str)>, hub: Option<&str>, key: &str) -> ContactRecord {
        ContactRecord {
            kind: "principal".into(),
            local: local.map(|(runtime, id)| LocalRoute {
                runtime: runtime.into(),
                id: id.into(),
            }),
            hub_id: hub.map(str::to_owned),
            identity_key: key.into(),
            signing_did: None,
            name: None,
            registration: "local_only".into(),
        }
    }

    #[tokio::test]
    async fn the_book_keys_contacts_by_immutable_key() {
        let dir = tempfile::tempdir().unwrap();
        let book = ContactBook::in_local_root(dir.path());
        assert!(book.list().unwrap().is_empty());
        book.remember(record(Some(("rt", "prin_a")), None, "k1"))
            .await
            .unwrap();
        book.remember(record(Some(("rt", "prin_a")), Some("hub-a"), "k1"))
            .await
            .unwrap();
        let listed = book.list().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].hub_id.as_deref(), Some("hub-a"));
    }

    /// ADR-081 D1: a local route is addressable only on its own runtime and
    /// only while the registry there holds the same key.
    #[tokio::test]
    async fn local_routes_are_reachable_only_at_home() {
        let dir = tempfile::tempdir().unwrap();
        let store = peko_channel::ChannelStore::new(peko_channel::ChannelConfig {
            runtime_dir: dir.path().into(),
            shared_dir: None,
        });
        let peer = Subject::Principal("prin_peer".into());
        let actor = store
            .register_actor(
                &peer,
                None,
                Some("did:key:zPeer"),
                Some("peer-key"),
                Some("peer"),
                "local_only",
            )
            .await
            .unwrap();
        let contact = ContactRecord::from_actor(&actor, "did:key:zHome");
        assert!(contact.reachable_on(&store, "did:key:zHome"));
        assert!(!contact.reachable_on(&store, "did:key:zAway"));
        assert_eq!(
            contact.subject_on("did:key:zAway").unwrap(),
            peer.qualified_for("did:key:zHome")
        );
        // A different actor under the same local ID is not this contact.
        let impostor = ContactRecord {
            identity_key: "other-key".into(),
            ..contact
        };
        assert!(!impostor.reachable_on(&store, "did:key:zHome"));
    }
}
