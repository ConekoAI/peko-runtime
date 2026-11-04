//! ADR-095 D7: while a runtime is registered, its person's DMs are keyed
//! by their Hub account, so a peko is one conversation with them on every
//! device.
//!
//! The channel store moves each DM whose pair id no longer matches the
//! person's current DM key ([`peko_channel::ChannelPort::rekey_local_user_dms`]);
//! this module makes the bound pekos' sessions follow — their channel
//! binding and their conversation compartment. It runs at boot, when the
//! tunnel is ready (registration connects it), and after unregistering,
//! and is idempotent: with nothing to move it does nothing.

use std::sync::Arc;

use peko_session::SessionManager;
use peko_tools_core::Compartment;
use tracing::{info, warn};

use super::manager::PrincipalManager;

/// Rekey the local user's DMs and rebind the sessions of the pekos in
/// them. Returns how many DMs moved.
///
/// # Errors
///
/// The channel store could not move a DM. A session that cannot be
/// rebound is logged and skipped.
pub async fn reconcile_local_user_dms(manager: &Arc<PrincipalManager>) -> anyhow::Result<usize> {
    let Some(port) = manager.channel_port() else {
        return Ok(0);
    };
    let moved = port.rekey_local_user_dms().await?;
    if moved.is_empty() {
        return Ok(0);
    }
    let principals = manager.list_all().await;
    for (old, new, bound) in &moved {
        info!(%old, %new, "DM rekeyed to the person's current address");
        for principal in principals.iter().filter(|p| bound.contains(&p.id.0)) {
            let mut sessions =
                SessionManager::new().with_sessions_dir_internal(principal.memory.sessions_dir());
            if let Err(e) = rebind(&mut sessions, old.as_str(), new.as_str()).await {
                warn!(principal = %principal.id, %old, %new, "sessions not rebound: {e}");
            }
        }
    }
    Ok(moved.len())
}

/// Every session bound to `old`, or in its compartment, moves to `new`.
async fn rebind(sessions: &mut SessionManager, old: &str, new: &str) -> anyhow::Result<()> {
    let old_compartment = Compartment::Conversation {
        channel: old.to_string(),
    };
    for meta in sessions.list_all_sessions(false).await? {
        let id = meta.session_id.to_string();
        if meta.channel_id.as_deref() == Some(old) {
            sessions.bind_session_channel(&id, new).await?;
        }
        if meta.compartment.as_ref() == Some(&old_compartment) {
            sessions
                .set_session_compartment(
                    &id,
                    Compartment::Conversation {
                        channel: new.to_string(),
                    },
                )
                .await?;
        }
    }
    Ok(())
}
