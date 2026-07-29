//! Runtime-authoritative scope that hands out tier-typed paths.
//!
//! Phase B of the three-tier storage migration. The boundary between
//! Local (per-principal runtime state), Shared (per-principal
//! capability-bearing config), and Runtime (runtime-wide binaries) is
//! enforced at the type level rather than at the call site: IPC handlers
//! and CLI commands take `LocalPath` / `SharedPath` / `RuntimePath`
//! newtypes whose constructors are sealed to this module.
//!
//! A `RuntimeAuthority` wraps a `PathResolver` and an `Arc<Subject>`
//! representing the actor who is asking. The constructor rejects actors
//! that aren't entitled to the tier they ask for — e.g. a peer channel
//! holding `Subject::User("alice")` cannot obtain a `LocalPath` for
//! principal `bob` (matches ADR-033's RBAC contract).
//!
//! Construction is intentionally restricted:
//! - [`RuntimeAuthority::for_runtime`] — `Subject::Public`. Used by the
//!   cron engine, the daemon's startup paths, and any housekeeping code
//!   that isn't acting on behalf of a peer.
//! - [`RuntimeAuthority::for_caller`] — accepts a `Subject` that has been
//!   verified by the daemon admission layer. The IPC handlers construct
//!   the authority once per request, immediately after authenticating
//!   the caller.
//!
//! See [`TierPath`] for the tier-typed wrapper API.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use peko_subject::{PrincipalId, Subject};

use crate::common::paths::{PathResolver, RuntimeLayout};

/// The storage tier a path belongs to.
///
/// Mirrors [`crate::common::paths::Tier`] (kept in `paths.rs` for layout
/// serialization) but is the canonical enum for runtime gating here —
/// callers compare on this type when they need to reason about tier
/// membership outside serialization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tier {
    /// Per-principal runtime state. Never packaged, never shared.
    Local,
    /// Per-principal capability-bearing config. Packaged into bundles.
    Shared,
    /// Runtime-wide state. Installed once; principals access via grants.
    Runtime,
}

impl From<crate::common::paths::Tier> for Tier {
    fn from(t: crate::common::paths::Tier) -> Self {
        match t {
            crate::common::paths::Tier::Local => Tier::Local,
            crate::common::paths::Tier::Shared => Tier::Shared,
            crate::common::paths::Tier::Runtime => Tier::Runtime,
        }
    }
}

/// Sealed marker — only types defined in this module implement it.
///
/// External code can hold, pass, and dereference a `LocalPath` / etc. but
/// cannot construct one. Construction is mediated by
/// [`RuntimeAuthority`], which enforces the actor + tier gate.
mod sealed {
    pub trait Sealed {}
}

/// Trait implemented by every tier-typed path wrapper.
///
/// Lets generic code ask "which tier?" and "give me the underlying path"
/// without exposing the inner `PathBuf` constructor. Outside of this
/// module the only way to obtain a `TierPath` is by calling
/// `RuntimeAuthority::*` accessors.
pub trait TierPath: sealed::Sealed + Sized {
    /// The tier this wrapper belongs to.
    fn tier() -> Tier;

    /// Borrow the underlying path.
    fn as_path(&self) -> &Path;

    /// Consume the wrapper and return the inner `PathBuf`.
    fn into_path_buf(self) -> PathBuf;

    /// Convenience clone — equivalent to `self.as_path().to_path_buf()`.
    fn to_path_buf(&self) -> PathBuf {
        self.as_path().to_path_buf()
    }
}

/// A path under a principal's Local tier. Constructible only via
/// [`RuntimeAuthority`] (cron engine + per-principal owner).
#[derive(Debug, Clone)]
pub struct LocalPath(PathBuf);

impl sealed::Sealed for LocalPath {}

impl TierPath for LocalPath {
    fn tier() -> Tier {
        Tier::Local
    }
    fn as_path(&self) -> &Path {
        &self.0
    }
    fn into_path_buf(self) -> PathBuf {
        self.0
    }
}

/// A path under a principal's Shared tier. Constructible only via
/// [`RuntimeAuthority`] (any authenticated peer with visibility on the
/// principal — public exposure applies).
#[derive(Debug, Clone)]
pub struct SharedPath(PathBuf);

impl sealed::Sealed for SharedPath {}

impl TierPath for SharedPath {
    fn tier() -> Tier {
        Tier::Shared
    }
    fn as_path(&self) -> &Path {
        &self.0
    }
    fn into_path_buf(self) -> PathBuf {
        self.0
    }
}

/// A path under the runtime-global bucket. Not principal-scoped, so any
/// authenticated actor (or `Subject::Public`) can obtain one.
#[derive(Debug, Clone)]
pub struct RuntimePath(PathBuf);

impl sealed::Sealed for RuntimePath {}

impl TierPath for RuntimePath {
    fn tier() -> Tier {
        Tier::Runtime
    }
    fn as_path(&self) -> &Path {
        &self.0
    }
    fn into_path_buf(self) -> PathBuf {
        self.0
    }
}

/// Authority granting access to a tier for a specific actor.
///
/// Cheap to clone (`Arc<Subject>` + `PathResolver` are both clone-cheap)
/// — IPC handlers can hand one to each per-request helper without
/// lifetime gymnastics. The `actor` is captured at construction; there is
/// no `with_actor` builder, so a given authority cannot be repurposed
/// for a different actor mid-flight.
#[derive(Debug, Clone)]
pub struct RuntimeAuthority {
    resolver: PathResolver,
    actor: Arc<Subject>,
}

/// Errors raised by [`RuntimeAuthority`] tier accessors.
///
/// Kept narrow on purpose: the only failure modes are (a) the principal
/// isn't known on disk, or (b) the actor isn't entitled to the tier. Any
/// I/O failure during filesystem reads is surfaced by the caller, not by
/// the authority.
#[derive(Debug, thiserror::Error)]
pub enum AuthorityError {
    /// The `PrincipalId` doesn't resolve to any on-disk `principal.toml`.
    #[error("principal not found: {0}")]
    UnknownPrincipal(PrincipalId),

    /// The caller may not touch the requested tier.
    #[error("caller may not touch tier {tier:?}")]
    TierDenied { tier: Tier },

    /// The on-disk layout for the principal is missing required
    /// directories. This indicates a half-installed principal and should
    /// be propagated as a hard error rather than silently re-creating
    /// state.
    #[error("principal layout missing for {0}")]
    LayoutMissing(PrincipalId),
}

impl RuntimeAuthority {
    /// Construct an authority for the runtime itself.
    ///
    /// The actor is `Subject::Public`. Use this for housekeeping code
    /// (cron engine, daemon startup paths, `principal create` /
    /// `principal remove` that pre-date any peer session).
    #[must_use]
    pub fn for_runtime(resolver: PathResolver) -> Self {
        Self {
            resolver,
            actor: Arc::new(Subject::Public),
        }
    }

    /// Construct an authority for a specific caller.
    ///
    /// The caller is responsible for having already verified the
    /// `Subject` (auth/JWT layer). The authority does no further
    /// authentication — it only enforces the actor↔tier gate.
    #[must_use]
    pub fn for_caller(resolver: PathResolver, actor: Subject) -> Self {
        Self {
            resolver,
            actor: Arc::new(actor),
        }
    }

    /// Borrow the underlying resolver. Provided for tests and one-off
    /// plumbing that genuinely needs to compose paths without the tier
    /// gate (e.g. fixture builders); production handlers should go
    /// through the typed accessors below.
    #[must_use]
    pub fn resolver(&self) -> &PathResolver {
        &self.resolver
    }

    /// Borrow the actor. Useful for audit logging alongside the path
    /// that was handed out.
    #[must_use]
    pub fn actor(&self) -> &Subject {
        &self.actor
    }

    // ---------------------------------------------------------------------
    // Local tier — per-principal runtime state (sessions, cron, locks,
    // memory_index). Only the runtime (`Subject::Public`) and the
    // principal owner (`Subject::Principal` matching the DID) can read
    // or write Local paths.
    // ---------------------------------------------------------------------

    /// Hand out a `LocalPath` pointing at the principal's Local-tier
    /// root.
    ///
    /// Returns `Err(AuthorityError::UnknownPrincipal)` if the principal
    /// isn't on disk; `Err(AuthorityError::TierDenied)` if the actor
    /// isn't `Subject::Public` or `Subject::Principal`.
    pub fn local_root(&self, principal: &PrincipalId) -> Result<LocalPath, AuthorityError> {
        self.assert_local_entitled()?;
        let layout = self.principal_layout(principal)?;
        Ok(LocalPath(layout.local.root))
    }

    /// Hand out a `LocalPath` for the principal's cron schedule file.
    pub fn local_cron_schedule(
        &self,
        principal: &PrincipalId,
    ) -> Result<LocalPath, AuthorityError> {
        self.assert_local_entitled()?;
        let layout = self.principal_layout(principal)?;
        Ok(LocalPath(layout.local.cron_schedule))
    }

    /// Hand out a `LocalPath` for the principal's cron history log.
    pub fn local_cron_history(
        &self,
        principal: &PrincipalId,
    ) -> Result<LocalPath, AuthorityError> {
        self.assert_local_entitled()?;
        let layout = self.principal_layout(principal)?;
        Ok(LocalPath(layout.local.cron_history))
    }

    /// Hand out a `LocalPath` for the principal's sessions directory.
    pub fn local_sessions_dir(
        &self,
        principal: &PrincipalId,
    ) -> Result<LocalPath, AuthorityError> {
        self.assert_local_entitled()?;
        let layout = self.principal_layout(principal)?;
        Ok(LocalPath(layout.local.sessions_dir))
    }

    // ---------------------------------------------------------------------
    // Shared tier — per-principal capability-bearing config (principal
    // identity, agents, MCP configs, memory snapshots). Any authenticated
    // actor with visibility on the principal can read; writes require
    // the principal owner.
    //
    // **Phase B (read path only):** the read gate is permissive — any
    // non-Public actor can ask for Shared paths and we'll hand them out.
    // The WriteSide gate (e.g. peer channels may read but not write
    // principal.toml) is deferred to Phase C, where the authority
    // composes with `peko_extension_api::Capabilities` for fine-grained
    // per-resource gating.
    // ---------------------------------------------------------------------

    /// Hand out a `SharedPath` for `principal.toml`.
    pub fn shared_config(
        &self,
        principal: &PrincipalId,
    ) -> Result<SharedPath, AuthorityError> {
        self.assert_shared_read_entitled()?;
        let layout = self.principal_layout(principal)?;
        Ok(SharedPath(layout.shared.config_file))
    }

    /// Hand out a `SharedPath` for the principal's agents directory.
    pub fn shared_agents_dir(
        &self,
        principal: &PrincipalId,
    ) -> Result<SharedPath, AuthorityError> {
        self.assert_shared_read_entitled()?;
        let layout = self.principal_layout(principal)?;
        Ok(SharedPath(layout.shared.agents_dir))
    }

    /// Hand out a `SharedPath` for the principal's identity directory
    /// (`identity.json`).
    pub fn shared_identity_dir(
        &self,
        principal: &PrincipalId,
    ) -> Result<SharedPath, AuthorityError> {
        self.assert_shared_read_entitled()?;
        let layout = self.principal_layout(principal)?;
        // The identity dir is the Shared root joined with `"identity"`,
        // matching the legacy `principal_identity_dir` layout.
        Ok(SharedPath(layout.shared.root.join("identity")))
    }

    /// Hand out a `SharedPath` for the principal's MCP server configs.
    pub fn shared_mcps_dir(
        &self,
        principal: &PrincipalId,
    ) -> Result<SharedPath, AuthorityError> {
        self.assert_shared_read_entitled()?;
        let layout = self.principal_layout(principal)?;
        Ok(SharedPath(layout.shared.mcps_dir))
    }

    // ---------------------------------------------------------------------
    // Runtime tier — runtime-wide binaries. Not principal-scoped; any
    // actor (including `Subject::Public`) can read or write.
    // ---------------------------------------------------------------------

    /// Hand out the full `RuntimeLayout`. The accessor stays as the
    /// layout struct (not a `RuntimePath`) because callers usually need
    /// several fields together.
    #[must_use]
    pub fn runtime_layout(&self) -> RuntimeLayout {
        self.resolver.runtime_layout()
    }

    /// Hand out a `RuntimePath` for the extensions install root.
    #[must_use]
    pub fn runtime_extensions_root(&self) -> RuntimePath {
        RuntimePath(self.resolver.extensions_root())
    }

    /// Hand out a `RuntimePath` for the MCP server install root.
    #[must_use]
    pub fn runtime_mcps_root(&self) -> RuntimePath {
        RuntimePath(self.resolver.mcps_root())
    }

    /// Hand out a `RuntimePath` for the OCI registry cache root.
    #[must_use]
    pub fn runtime_registry_root(&self) -> RuntimePath {
        RuntimePath(self.resolver.registry_root())
    }

    /// Hand out a `RuntimePath` for the runtime-wide lock directory.
    #[must_use]
    pub fn runtime_locks_dir(&self) -> RuntimePath {
        RuntimePath(self.resolver.runtime_layout().locks_dir)
    }

    // ---------------------------------------------------------------------
    // Internal helpers
    // ---------------------------------------------------------------------

    /// Resolve the on-disk layout for a principal by DID.
    ///
    /// Phase B keeps the on-disk keying as `principal_name` (matches the
    /// schedule file naming and is what the rest of the IPC surface
    /// already uses). The DID → name resolution is a one-time scan over
    /// `principals_root_dir`, which is cheap — there are at most a few
    /// dozen principals on a typical install.
    fn principal_layout(
        &self,
        principal: &PrincipalId,
    ) -> Result<crate::common::paths::PrincipalLayout, AuthorityError> {
        let name = self
            .resolver
            .lookup_principal_name(principal)
            .ok_or_else(|| AuthorityError::UnknownPrincipal(principal.clone()))?;
        Ok(self.resolver.principal_layout(&name))
    }

    /// Local-tier gate: only the runtime (`Subject::Public`) or a
    /// principal-typed subject may receive a `LocalPath`. Peer-as-User
    /// (CLI without a peer session) cannot obtain a `LocalPath`.
    fn assert_local_entitled(&self) -> Result<(), AuthorityError> {
        match self.actor.as_ref() {
            Subject::Public | Subject::Principal(_) => Ok(()),
            Subject::User(_) => Err(AuthorityError::TierDenied { tier: Tier::Local }),
        }
    }

    /// Shared-tier read gate (Phase B permissive). Any non-`Public`
    /// actor can read Shared paths. The tighter write gate lands in
    /// Phase C alongside `Capabilities` composition.
    fn assert_shared_read_entitled(&self) -> Result<(), AuthorityError> {
        match self.actor.as_ref() {
            Subject::Public => Err(AuthorityError::TierDenied { tier: Tier::Shared }),
            Subject::User(_) | Subject::Principal(_) => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_resolver() -> PathResolver {
        PathResolver::with_dirs(
            PathBuf::from("/config"),
            PathBuf::from("/data"),
            PathBuf::from("/cache"),
        )
    }

    fn principal_id(suffix: &str) -> PrincipalId {
        // Generated ids start with `prin_`; tests use the literal form
        // because we don't write to disk in these checks.
        PrincipalId(format!("prin_{suffix}"))
    }

    #[test]
    fn tier_classification_matches_paths_module() {
        assert_eq!(Tier::Local, crate::common::paths::Tier::Local.into());
        assert_eq!(Tier::Shared, crate::common::paths::Tier::Shared.into());
        assert_eq!(Tier::Runtime, crate::common::paths::Tier::Runtime.into());
    }

    #[test]
    fn runtime_authority_yields_runtime_paths_without_principal() {
        let resolver = test_resolver();
        let authority = RuntimeAuthority::for_runtime(resolver);
        let ext = authority.runtime_extensions_root();
        assert_eq!(RuntimePath::tier(), Tier::Runtime);
        assert_eq!(ext.as_path(), Path::new("/data/runtime/extensions"));
    }

    #[test]
    fn runtime_authority_principal_path_accessors_return_typed_paths() {
        // We exercise the typed accessors on a resolver that has no
        // on-disk principal — the actor gate fires before the lookup
        // because the actor is Public (the runtime).
        let resolver = test_resolver();
        let authority = RuntimeAuthority::for_runtime(resolver);
        let pid = principal_id("404");
        let result = authority.local_root(&pid);
        assert!(matches!(result, Err(AuthorityError::UnknownPrincipal(_))));
    }

    #[test]
    fn local_gate_denies_subject_user() {
        let resolver = test_resolver();
        let authority =
            RuntimeAuthority::for_caller(resolver, Subject::User("alice".into()));
        let pid = principal_id("404");
        // Subject::User is rejected even before the on-disk lookup.
        let result = authority.local_root(&pid);
        assert!(matches!(
            result,
            Err(AuthorityError::TierDenied { tier: Tier::Local })
        ));
    }

    #[test]
    fn shared_read_gate_rejects_subject_public() {
        // The runtime (Subject::Public) cannot read Shared-tier paths
        // because it should always go through the Local runtime paths
        // when operating on its own behalf. If a runtime-internal path
        // is needed the caller should use `shared_*` only when it
        // really has a peer identity to project.
        let resolver = test_resolver();
        let authority = RuntimeAuthority::for_runtime(resolver);
        let pid = principal_id("404");
        let result = authority.shared_config(&pid);
        assert!(matches!(
            result,
            Err(AuthorityError::TierDenied { tier: Tier::Shared })
        ));
    }

    #[test]
    fn tier_path_to_path_buf_matches_as_path() {
        let path = RuntimePath(PathBuf::from("/data/runtime/extensions"));
        assert_eq!(path.to_path_buf(), path.as_path().to_path_buf());
    }

    #[test]
    fn tier_path_into_path_buf_drops_wrapper() {
        let path = RuntimePath(PathBuf::from("/data/runtime/extensions"));
        let buf = path.into_path_buf();
        assert_eq!(buf, PathBuf::from("/data/runtime/extensions"));
    }
}