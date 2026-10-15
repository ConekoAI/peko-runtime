//! Actor references across runtimes (ADR-081 D6).
//!
//! A snapshot leaves the runtime that hosts the principal, so every actor
//! reference in its identity-bearing state — the owner, grants, `POLICY.md`
//! sections, and cron creators — is rewritten on the way out and on the
//! way in, by one rule:
//!
//! - **Export:** an actor with a Hub route is written by that route
//!   (portable everywhere); a local-only actor is written qualified with
//!   the source runtime (`kind:local:<id>@<runtime DID>`), which matches no
//!   local actor anywhere else.
//! - **Import:** a route qualified with the destination runtime becomes an
//!   ordinary local route again (it revives when the principal returns);
//!   other qualified routes stay dormant. A local-only owner from another
//!   runtime is replaced by the destination's local user, everywhere it is
//!   referenced.

use std::collections::HashMap;

use peko_subject::Subject;

use crate::principal::config::PrincipalConfig;

/// Rewrite one reference.
pub type Rewrite<'a> = dyn Fn(&Subject) -> Subject + Send + Sync + 'a;

/// The export rule, given the Hub routes of the actors that have one
/// (keyed by canonical address).
pub fn export_rule<'a>(
    source_runtime: &'a str,
    hub_routes: &'a HashMap<String, Subject>,
) -> impl Fn(&Subject) -> Subject + 'a {
    move |subject| {
        hub_routes
            .get(&subject.canonical_address())
            .cloned()
            .unwrap_or_else(|| subject.qualified_for(source_runtime))
    }
}

/// The import rule on `destination`. `foreign_owner` is the snapshot's
/// owner when it is a local-only actor of another runtime: it becomes the
/// destination's local user.
pub fn import_rule<'a>(
    destination: &'a str,
    foreign_owner: Option<&'a Subject>,
) -> impl Fn(&Subject) -> Subject + 'a {
    move |subject| {
        if foreign_owner
            .is_some_and(|owner| owner.canonical_address() == subject.canonical_address())
        {
            return Subject::User("local".into());
        }
        subject.localized_on(destination)
    }
}

/// The owner a snapshot names when it is a local-only actor of a runtime
/// other than `destination` (ADR-081 D6 step 2).
#[must_use]
pub fn foreign_local_owner(config: &PrincipalConfig, destination: &str) -> Option<Subject> {
    let owner = &config.owner;
    owner
        .route_runtime()
        .filter(|runtime| *runtime != destination)
        .map(|_| owner.clone())
}

/// The owner and every grant's subject and granter.
pub fn rewrite_config(config: &mut PrincipalConfig, rewrite: &Rewrite<'_>) {
    if !config.owner.subject_id().is_empty() {
        config.owner = rewrite(&config.owner);
    }
    for grant in &mut config.permissions {
        if grant.subject != Subject::Public {
            grant.subject = rewrite(&grant.subject);
        }
        if !grant.granted_by.subject_id().is_empty() {
            grant.granted_by = rewrite(&grant.granted_by);
        }
    }
}

/// `## <actor address>` section headings of `POLICY.md`. Headings that do
/// not name an actor (group channels, prose) are kept as written.
#[must_use]
pub fn rewrite_policy(text: &str, rewrite: &Rewrite<'_>) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let rewritten = line.strip_prefix("## ").and_then(|heading| {
            let (address, rest) = heading
                .split_once(char::is_whitespace)
                .map_or((heading.trim_end(), ""), |(a, r)| (a, r));
            let subject = address.parse::<Subject>().ok()?;
            let ending = if line.ends_with('\n') && !rest.ends_with('\n') {
                "\n"
            } else {
                ""
            };
            let separator = if rest.is_empty() { "" } else { " " };
            Some(format!(
                "## {}{separator}{rest}{ending}",
                rewrite(&subject).canonical_address()
            ))
        });
        out.push_str(rewritten.as_deref().unwrap_or(line));
    }
    out
}

/// Every cron job's `created_by.actor` in the cron database (JSON).
#[must_use]
pub fn rewrite_cron(bytes: &[u8], rewrite: &Rewrite<'_>) -> Vec<u8> {
    let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return bytes.to_vec();
    };
    let Some(jobs) = value.get_mut("jobs").and_then(|j| j.as_array_mut()) else {
        return bytes.to_vec();
    };
    for job in jobs {
        let Some(actor) = job.get_mut("created_by").and_then(|c| c.get_mut("actor")) else {
            continue;
        };
        if let Some(subject) = actor.as_str().and_then(|a| a.parse::<Subject>().ok()) {
            *actor = serde_json::Value::String(rewrite(&subject).canonical_address());
        }
    }
    serde_json::to_vec_pretty(&value).unwrap_or_else(|_| bytes.to_vec())
}

/// Every actor reference [`rewrite_config`], [`rewrite_policy`], and
/// [`rewrite_cron`] would touch, for computing Hub routes before export.
#[must_use]
pub fn referenced_actors(
    config: &PrincipalConfig,
    policy: Option<&str>,
    cron: Option<&[u8]>,
) -> Vec<Subject> {
    let seen = std::sync::Mutex::new(vec![config.owner.clone()]);
    for grant in &config.permissions {
        seen.lock().expect("collector").push(grant.subject.clone());
        seen.lock()
            .expect("collector")
            .push(grant.granted_by.clone());
    }
    let collect = |subject: &Subject| {
        seen.lock().expect("collector").push(subject.clone());
        subject.clone()
    };
    if let Some(policy) = policy {
        let _ = rewrite_policy(policy, &collect);
    }
    if let Some(cron) = cron {
        let _ = rewrite_cron(cron, &collect);
    }
    let mut out = seen.into_inner().expect("collector");
    out.retain(|s| *s != Subject::Public && !s.subject_id().is_empty());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use peko_auth::{Permission, PermissionGrant};

    const SRC: &str = "did:key:zSource";
    const DST: &str = "did:key:zDest";

    fn grant(subject: &str) -> PermissionGrant {
        PermissionGrant {
            subject: subject.parse().unwrap(),
            permission: Permission::Trusted,
            granted_at: String::new(),
            granted_by: Subject::User("local".into()),
        }
    }

    fn config(owner: &str, grants: &[&str]) -> PrincipalConfig {
        let mut config: PrincipalConfig =
            serde_json::from_value(serde_json::json!({"name": "p"})).unwrap();
        config.owner = owner.parse().unwrap();
        config.permissions = grants.iter().map(|g| grant(g)).collect();
        config
    }

    /// ADR-081 D6: a Hub owner is unchanged across a move; a local owner
    /// becomes the destination's local user; local grants go dormant away
    /// from home and revive on return.
    #[test]
    fn owners_and_grants_follow_the_move_rules() {
        let mut hub_routes = HashMap::new();
        hub_routes.insert(
            "user:local:local".to_string(),
            "user:hub:alice".parse::<Subject>().unwrap(),
        );
        // Registered local user: exported by its Hub route.
        let mut registered = config("user:local", &["principal:local:prin_peer"]);
        rewrite_config(&mut registered, &export_rule(SRC, &hub_routes));
        assert_eq!(registered.owner, "user:hub:alice".parse().unwrap());
        assert_eq!(registered.permissions[0].granted_by, registered.owner);
        assert_eq!(
            registered.permissions[0].subject,
            "principal:local:prin_peer@did:key:zSource".parse().unwrap()
        );
        let owner = foreign_local_owner(&registered, DST);
        rewrite_config(&mut registered, &import_rule(DST, owner.as_ref()));
        assert_eq!(
            registered.owner,
            "user:hub:alice".parse().unwrap(),
            "Hub owner unchanged"
        );
        let keys = peko_subject::CanonicalKeys;
        use peko_subject::ActorKeys;
        assert!(
            !keys.same_actor(
                &registered.permissions[0].subject,
                &"principal:local:prin_peer".parse().unwrap()
            ),
            "a foreign local grant is dormant"
        );

        // Unregistered local user: exported qualified, reassigned on import.
        let none = HashMap::new();
        let mut offline = config("user:local", &["principal:local:prin_peer"]);
        rewrite_config(&mut offline, &export_rule(SRC, &none));
        assert_eq!(
            offline.owner,
            "user:local:local@did:key:zSource".parse().unwrap()
        );
        let owner = foreign_local_owner(&offline, DST);
        rewrite_config(&mut offline, &import_rule(DST, owner.as_ref()));
        assert_eq!(offline.owner, Subject::User("local".into()));
        assert_eq!(
            offline.permissions[0].granted_by,
            Subject::User("local".into())
        );

        // Back home, everything revives.
        let mut home = config("user:local", &["principal:local:prin_peer"]);
        rewrite_config(&mut home, &export_rule(SRC, &none));
        let owner = foreign_local_owner(&home, SRC);
        assert!(owner.is_none(), "an owner of this runtime is not foreign");
        rewrite_config(&mut home, &import_rule(SRC, None));
        assert!(keys.same_actor(&home.owner, &Subject::User("local".into())));
        assert!(keys.same_actor(
            &home.permissions[0].subject,
            &"principal:local:prin_peer".parse().unwrap()
        ));
    }

    #[test]
    fn policy_headings_and_cron_creators_are_rewritten() {
        let none = HashMap::new();
        let rule = export_rule(SRC, &none);
        let policy = "# Defaults\n- be nice\n\n## principal:local:prin_x (Bob)\n- ok\n## grp_team\n- group\n## user:hub:alice\n";
        let out = rewrite_policy(policy, &rule);
        assert!(
            out.contains("## principal:local:prin_x@did:key:zSource (Bob)\n"),
            "{out}"
        );
        assert!(out.contains("## grp_team\n"), "{out}");
        assert!(out.contains("## user:hub:alice\n"), "{out}");
        assert!(out.contains("# Defaults\n- be nice\n"), "{out}");

        let cron = serde_json::json!({"jobs": [
            {"id": "a", "created_by": {"actor": "user:local:local", "trust": "owner"}},
            {"id": "b"},
        ]});
        let out: serde_json::Value =
            serde_json::from_slice(&rewrite_cron(&serde_json::to_vec(&cron).unwrap(), &rule))
                .unwrap();
        assert_eq!(
            out["jobs"][0]["created_by"]["actor"],
            "user:local:local@did:key:zSource"
        );
        assert!(out["jobs"][1].get("created_by").is_none());
    }
}
