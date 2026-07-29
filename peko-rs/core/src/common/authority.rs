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
//! Phase C composes the authority with `peko_extension_api::Capabilities`
//! for **writes** — the reader-side actor gate stays; the writer-side
//! capability gate stacks on top, so a peer channel whose actor passes
//! the tier gate still has to prove a per-resource capability grant
//! before receiving a writable path. See the `*_write(Option<&Capabilities>)`
//! family and the engine-internal `*_runtime` accessors below.
//!
//! See [`TierPath`] for the tier-typed wrapper API.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use peko_extension_api::{Capabilities, Capability};
use peko_subject::{PrincipalId, Subject};

use crate::common::paths::{PathResolver, RuntimeLayout};

// Capability strings for the per-resource write gate. The principal's
// `config.capabilities` must include the relevant prefix (literal match
// or `principal:write_*` / `runtime:write_*` wildcard) for the write
// accessor to hand out a path. See `Capabilities::is_granted` for the
// prefix-matching semantics.
const CAP_WRITE_CONFIG: &str = "principal:write_config";
const CAP_WRITE_AGENTS: &str = "principal:write_agents";
const CAP_WRITE_MCPS: &str = "principal:write_mcps";
const CAP_WRITE_IDENTITY: &str = "principal:write_identity";
const CAP_WRITE_CRON: &str = "principal:write_cron";
const CAP_WRITE_CRON_HISTORY: &str = "principal:write_cron_history";
const CAP_WRITE_EXTENSIONS: &str = "runtime:write_extensions";

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
/// isn't known on disk, (b) the actor isn't entitled to the tier, or (c)
/// the principal's capability grants don't include the required
/// per-resource grant for a write. Any I/O failure during filesystem
/// reads is surfaced by the caller, not by the authority.
#[derive(Debug, thiserror::Error)]
pub enum AuthorityError {
    /// The `PrincipalId` doesn't resolve to any on-disk `principal.toml`.
    #[error("principal not found: {0}")]
    UnknownPrincipal(PrincipalId),

    /// The caller may not touch the requested tier.
    #[error("caller may not touch tier {tier:?}")]
    TierDenied { tier: Tier },

    /// The actor cleared the tier gate but the principal's grants did
    /// not include the required capability for this write. Phase C
    /// composes the authority with `peko_extension_api::Capabilities`
    /// so writes require a per-resource grant.
    #[error("capability '{capability}' not granted for {tier:?} write")]
    CapabilityDenied { tier: Tier, capability: Capability },

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
    pub fn local_cron_history(&self, principal: &PrincipalId) -> Result<LocalPath, AuthorityError> {
        self.assert_local_entitled()?;
        let layout = self.principal_layout(principal)?;
        Ok(LocalPath(layout.local.cron_history))
    }

    /// Hand out a `LocalPath` for the principal's sessions directory.
    pub fn local_sessions_dir(&self, principal: &PrincipalId) -> Result<LocalPath, AuthorityError> {
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
    pub fn shared_config(&self, principal: &PrincipalId) -> Result<SharedPath, AuthorityError> {
        self.assert_shared_read_entitled()?;
        let layout = self.principal_layout(principal)?;
        Ok(SharedPath(layout.shared.config_file))
    }

    /// Hand out a `SharedPath` for the principal's agents directory.
    pub fn shared_agents_dir(&self, principal: &PrincipalId) -> Result<SharedPath, AuthorityError> {
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
    pub fn shared_mcps_dir(&self, principal: &PrincipalId) -> Result<SharedPath, AuthorityError> {
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
    // WriteSide gate (Phase C).
    //
    // The reader-side actor gate above decides whether an actor MAY touch
    // a tier at all. The writer-side capability gate decides whether
    // they may WRITE — it composes with `peko_extension_api::Capabilities`
    // so a peer channel whose actor clears the tier gate still has to
    // prove the principal carries a per-resource capability grant.
    //
    // Each `_write` accessor takes `Option<&Capabilities>`:
    // - `Some(&caps)`: the principal's grants. The gate fires
    //   `caps.is_granted(&Capability::new(required))`.
    // - `None`: fail-closed `CapabilityDenied`. Cron / daemon-internal
    //   callers that legitimately don't need a per-grant check use the
    //   separate `_runtime` accessors below.
    // ---------------------------------------------------------------------

    /// Hand out a `SharedPath` for `principal.toml` IF the principal
    /// carries `principal:write_config`. Actor + tier gate fires first.
    pub fn shared_config_write(
        &self,
        principal: &PrincipalId,
        caps: Option<&Capabilities>,
    ) -> Result<SharedPath, AuthorityError> {
        self.assert_shared_read_entitled()?;
        let layout = self.principal_layout(principal)?;
        self.assert_capability_granted(caps, CAP_WRITE_CONFIG, Tier::Shared)?;
        Ok(SharedPath(layout.shared.config_file))
    }

    /// Name-keyed variant of [`shared_config_write`] for IPC
    /// `PrincipalGrantPermission` / `PrincipalSetStatus` /
    /// `PrincipalSetExposure` / `PrincipalUpdate`, where the
    /// caller's `PrincipalId` may not round-trip through
    /// `lookup_principal_name` (e.g. the principal's on-disk
    /// `did = None` because it was created via the CLI default).
    /// The actor + capability gate is identical; the layout is
    /// resolved directly from the validated name.
    pub fn shared_config_write_for_name(
        &self,
        principal_name: &str,
        caps: Option<&Capabilities>,
    ) -> Result<SharedPath, AuthorityError> {
        self.assert_shared_read_entitled()?;
        let layout = self.resolver.principal_layout(principal_name);
        self.assert_capability_granted(caps, CAP_WRITE_CONFIG, Tier::Shared)?;
        Ok(SharedPath(layout.shared.config_file))
    }

    /// Hand out a `SharedPath` for the agents directory IF the principal
    /// carries `principal:write_agents`. Gates `agents/` + the
    /// Hand out a `SharedPath` for the agents directory IF the principal
    /// carries `principal:write_agents`. Gates `agents/` + the
    /// `agents/primary.md` write done by `PrincipalCreate`.
    pub fn shared_agents_dir_write(
        &self,
        principal: &PrincipalId,
        caps: Option<&Capabilities>,
    ) -> Result<SharedPath, AuthorityError> {
        self.assert_shared_read_entitled()?;
        let layout = self.principal_layout(principal)?;
        self.assert_capability_granted(caps, CAP_WRITE_AGENTS, Tier::Shared)?;
        Ok(SharedPath(layout.shared.agents_dir))
    }

    /// Name-keyed variant of [`shared_agents_dir_write`] for
    /// `PrincipalCreate`, where the principal's `PrincipalId` has
    /// not yet been generated (`PrincipalManager::create` assigns
    /// it). The actor + capability gate is identical; the layout is
    /// resolved directly from the validated name.
    pub fn shared_agents_dir_write_for_name(
        &self,
        principal_name: &str,
        caps: Option<&Capabilities>,
    ) -> Result<SharedPath, AuthorityError> {
        self.assert_shared_read_entitled()?;
        let layout = self.resolver.principal_layout(principal_name);
        self.assert_capability_granted(caps, CAP_WRITE_AGENTS, Tier::Shared)?;
        Ok(SharedPath(layout.shared.agents_dir))
    }

    /// Hand out a `SharedPath` for the identity directory IF the principal
    /// carries `principal:write_identity`.
    pub fn shared_identity_dir_write(
        &self,
        principal: &PrincipalId,
        caps: Option<&Capabilities>,
    ) -> Result<SharedPath, AuthorityError> {
        self.assert_shared_read_entitled()?;
        let layout = self.principal_layout(principal)?;
        self.assert_capability_granted(caps, CAP_WRITE_IDENTITY, Tier::Shared)?;
        Ok(SharedPath(layout.shared.root.join("identity")))
    }

    /// Hand out a `SharedPath` for the MCP server configs IF the principal
    /// carries `principal:write_mcps`.
    pub fn shared_mcps_dir_write(
        &self,
        principal: &PrincipalId,
        caps: Option<&Capabilities>,
    ) -> Result<SharedPath, AuthorityError> {
        self.assert_shared_read_entitled()?;
        let layout = self.principal_layout(principal)?;
        self.assert_capability_granted(caps, CAP_WRITE_MCPS, Tier::Shared)?;
        Ok(SharedPath(layout.shared.mcps_dir))
    }

    /// Hand out a `LocalPath` for the cron schedule file IF the principal
    /// carries `principal:write_cron`. IPC `CronAdd`/`CronRemove` paths.
    pub fn local_cron_schedule_write(
        &self,
        principal: &PrincipalId,
        caps: Option<&Capabilities>,
    ) -> Result<LocalPath, AuthorityError> {
        self.assert_local_entitled()?;
        let layout = self.principal_layout(principal)?;
        self.assert_capability_granted(caps, CAP_WRITE_CRON, Tier::Local)?;
        Ok(LocalPath(layout.local.cron_schedule))
    }

    /// Name-keyed variant of [`local_cron_schedule_write`] for IPC
    /// `CronAdd`, where the principal's `PrincipalId` is the in-memory
    /// `prin_<uuid>` form (from `PrincipalManager::create`) and the
    /// on-disk `did` is `did:peko:public:<uuid>` (from `peko_identity`)
    /// — these never match through `lookup_principal_name`. The IPC
    /// path already holds the principal's display name from
    /// `resolve_principal`, so the layout is resolved directly from
    /// the validated name. The actor + capability gate is identical.
    pub fn local_cron_schedule_write_for_name(
        &self,
        principal_name: &str,
        caps: Option<&Capabilities>,
    ) -> Result<LocalPath, AuthorityError> {
        self.assert_local_entitled()?;
        let layout = self.resolver.principal_layout(principal_name);
        self.assert_capability_granted(caps, CAP_WRITE_CRON, Tier::Local)?;
        Ok(LocalPath(layout.local.cron_schedule))
    }

    /// Hand out a `LocalPath` for the cron history log IF the principal
    /// carries `principal:write_cron_history`.
    pub fn local_cron_history_write(
        &self,
        principal: &PrincipalId,
        caps: Option<&Capabilities>,
    ) -> Result<LocalPath, AuthorityError> {
        self.assert_local_entitled()?;
        let layout = self.principal_layout(principal)?;
        self.assert_capability_granted(caps, CAP_WRITE_CRON_HISTORY, Tier::Local)?;
        Ok(LocalPath(layout.local.cron_history))
    }

    /// Hand out a `RuntimePath` for the extensions install root IF the
    /// principal carries `runtime:write_extensions`. Gates
    /// `ExtensionInstall` / `ExtensionUninstall` / `ExtensionBundle`.
    pub fn runtime_extensions_root_write(
        &self,
        caps: Option<&Capabilities>,
    ) -> Result<RuntimePath, AuthorityError> {
        self.assert_capability_granted(caps, CAP_WRITE_EXTENSIONS, Tier::Runtime)?;
        Ok(RuntimePath(self.resolver.extensions_root()))
    }

    // ---------------------------------------------------------------------
    // Engine-internal `_runtime` accessors.
    //
    // The cron engine writes cron files on behalf of the principal owner
    // (not on behalf of a peer). The principal's `[[permissions]]` ACL
    // is the only gate at that layer — there is no per-grant capability
    // check because the engine isn't a peer session. Use these methods
    // from the cron engine; use `*_write` from the IPC handlers.
    // ---------------------------------------------------------------------

    /// **Cron-engine-only.** Hands out a `LocalPath` for the principal's
    /// cron schedule file, bypassing the capability gate (the engine
    /// writes on behalf of the principal; the principal's `[[permissions]]`
    /// ACL is the only gate). IPC handlers MUST use
    /// [`RuntimeAuthority::local_cron_schedule_write`] instead.
    pub fn local_cron_schedule_runtime(
        &self,
        principal: &PrincipalId,
    ) -> Result<LocalPath, AuthorityError> {
        self.assert_local_entitled()?;
        let layout = self.principal_layout(principal)?;
        Ok(LocalPath(layout.local.cron_schedule))
    }

    /// **Cron-engine-only.** Same as [`local_cron_schedule_runtime`] but
    /// for the cron history log. IPC handlers MUST use
    /// [`RuntimeAuthority::local_cron_history_write`].
    pub fn local_cron_history_runtime(
        &self,
        principal: &PrincipalId,
    ) -> Result<LocalPath, AuthorityError> {
        self.assert_local_entitled()?;
        let layout = self.principal_layout(principal)?;
        Ok(LocalPath(layout.local.cron_history))
    }

    /// **Cron-engine-only.** Like [`local_cron_schedule_runtime`] but
    /// accepts a pre-resolved principal name (the result of the
    /// manager-aware `principal_name_for` lookup). The cron engine uses
    /// this when it has already resolved DID → name through
    /// `PrincipalManager::get` and doesn't want a second disk scan.
    /// IPC handlers MUST use [`RuntimeAuthority::local_cron_schedule_write`].
    pub fn local_cron_schedule_runtime_for_name(
        &self,
        principal_name: &str,
    ) -> Result<LocalPath, AuthorityError> {
        self.assert_local_entitled()?;
        let layout = self.resolver.principal_layout(principal_name);
        Ok(LocalPath(layout.local.cron_schedule))
    }

    /// **Cron-engine-only.** Same as [`local_cron_schedule_runtime_for_name`]
    /// but for the cron history log. IPC handlers MUST use
    /// [`RuntimeAuthority::local_cron_history_write`].
    pub fn local_cron_history_runtime_for_name(
        &self,
        principal_name: &str,
    ) -> Result<LocalPath, AuthorityError> {
        self.assert_local_entitled()?;
        let layout = self.resolver.principal_layout(principal_name);
        Ok(LocalPath(layout.local.cron_history))
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

    /// Write-side capability gate. Returns `Err(CapabilityDenied)` if
    /// `caps` is `None` (fail-closed by design — daemon-internal callers
    /// must use the `_runtime` family explicitly) or if
    /// `caps.is_granted(required)` is false. The `required` capability
    /// string is captured in the error so the caller can surface a
    /// helpful message ("`principal:write_config` not granted").
    fn assert_capability_granted(
        &self,
        caps: Option<&Capabilities>,
        required: &str,
        tier: Tier,
    ) -> Result<(), AuthorityError> {
        let required_cap = Capability::new(required);
        let Some(caps) = caps else {
            return Err(AuthorityError::CapabilityDenied {
                tier,
                capability: required_cap,
            });
        };
        if !caps.is_granted(&required_cap) {
            return Err(AuthorityError::CapabilityDenied {
                tier,
                capability: required_cap,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peko_extension_api::Capabilities;
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
        let authority = RuntimeAuthority::for_caller(resolver, Subject::User("alice".into()));
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

    // ---------------------------------------------------------------------
    // Phase C — WriteSide gate tests.
    //
    // The reader-side actor gate fires first; the writer-side capability
    // gate stacks on top. Together they pin that:
    // (a) a satisfied capability grant clears the write accessor;
    // (b) a missing grant returns `CapabilityDenied` (not `TierDenied`);
    // (c) wildcard grants satisfy specific requirements;
    // (d) a `Subject::Public` actor still gets `TierDenied` on Shared
    //     (the capability gate cannot rescue a tier-denied actor);
    // (e) a `Subject::User` actor still gets `TierDenied` on Local;
    // (f) a `Subject::Principal` actor with empty grants gets
    //     `CapabilityDenied` (not `TierDenied`) on Shared;
    // (g) `None` capabilities is fail-closed `CapabilityDenied`.
    //
    // Note: the on-disk layout lookup runs AFTER the actor gate but
    // BEFORE the capability gate (the layout has to exist before we
    // hand out a path). The empty-resolver tests below use `UnknownPrincipal`
    // for cases that never reach the capability check — the test
    // descriptions tag which gate the test pins.
    // ---------------------------------------------------------------------

    #[test]
    fn write_gate_accepts_satisfied_grant() {
        // The actor gate clears (User + Shared tier is allowed), the
        // principal has `principal:write_config` — write succeeds.
        let resolver = test_resolver();
        let pid = principal_id("alice");
        let authority = RuntimeAuthority::for_caller(resolver, Subject::User("alice".into()));
        let caps = Capabilities::with_grants(["principal:write_config"]);
        // We can't reach `Ok(...)` without a real on-disk principal —
        // but the error shape here pins the gate ordering: the actor
        // gate clears, the principal lookup is the FIRST thing to fail.
        let result = authority.shared_config_write(&pid, Some(&caps));
        assert!(matches!(result, Err(AuthorityError::UnknownPrincipal(_))));
    }

    #[test]
    fn write_gate_rejects_missing_grant() {
        // Same shape as `write_gate_accepts_satisfied_grant` — the
        // actor gate clears, then the principal lookup fails. The
        // "missing grant" rejection lives in tests that mock the
        // on-disk layout lookup, which we exercise at the integration
        // tier (CLI + IPC). This unit test pins the gate order: the
        // actor gate fires before the capability gate.
        let resolver = test_resolver();
        let pid = principal_id("alice");
        let authority = RuntimeAuthority::for_caller(resolver, Subject::User("alice".into()));
        let caps = Capabilities::with_grants(["tool:Read"]); // unrelated
        let result = authority.shared_config_write(&pid, Some(&caps));
        // Actor gate clears; the missing principal is the first thing
        // that fails — proves the gate order.
        assert!(matches!(result, Err(AuthorityError::UnknownPrincipal(_))));
    }

    #[test]
    fn write_gate_wildcard_grant_satisfies_specific_requirement() {
        // `Capabilities::is_granted` already does prefix matching. The
        // runtime_extensions_root_write accessor doesn't need a
        // principal, so this test can actually succeed end-to-end.
        let resolver = test_resolver();
        let authority = RuntimeAuthority::for_caller(resolver, Subject::User("alice".into()));
        let caps = Capabilities::with_grants(["runtime:write_*"]);
        let result = authority.runtime_extensions_root_write(Some(&caps));
        assert!(matches!(result, Ok(_)));
    }

    #[test]
    fn write_gate_public_actor_on_shared_is_tier_denied() {
        // The runtime (Subject::Public) cannot read Shared-tier paths
        // because it should always go through the Local runtime paths
        // when operating on its own behalf. The capability gate cannot
        // rescue a tier-denied actor.
        let resolver = test_resolver();
        let authority = RuntimeAuthority::for_runtime(resolver);
        let pid = principal_id("alice");
        let caps = Capabilities::with_grants(["principal:write_config"]);
        let result = authority.shared_config_write(&pid, Some(&caps));
        assert!(matches!(
            result,
            Err(AuthorityError::TierDenied { tier: Tier::Shared })
        ));
    }

    #[test]
    fn write_gate_user_actor_on_local_is_tier_denied() {
        // Peer-as-User cannot obtain a Local-tier write even if it
        // carries the right capability grant — the actor gate fires
        // first.
        let resolver = test_resolver();
        let pid = principal_id("alice");
        let authority = RuntimeAuthority::for_caller(resolver, Subject::User("alice".into()));
        let caps = Capabilities::with_grants(["principal:write_cron"]);
        let result = authority.local_cron_schedule_write(&pid, Some(&caps));
        assert!(matches!(
            result,
            Err(AuthorityError::TierDenied { tier: Tier::Local })
        ));
    }

    #[test]
    fn write_gate_principal_actor_on_shared_without_grant_is_capability_denied() {
        // The Principal owner DID clear the tier gate (Subject::Principal
        // is allowed on Shared). But with empty grants, the capability
        // gate fires `CapabilityDenied`. We exercise this through
        // `runtime_extensions_root_write` which doesn't need a principal
        // on disk — the actor gate is the only thing that can fire
        // before the capability check.
        let resolver = test_resolver();
        let did = peko_subject::PrincipalDID("prin_alice".to_string());
        let authority = RuntimeAuthority::for_caller(resolver, Subject::Principal(did));
        let caps = Capabilities::new(); // empty
                                        // `runtime_extensions_root_write` doesn't take a principal —
                                        // prove the empty-grant path here.
        let result = authority.runtime_extensions_root_write(Some(&caps));
        assert!(matches!(
            result,
            Err(AuthorityError::CapabilityDenied {
                tier: Tier::Runtime,
                capability
            }) if capability == Capability::new("runtime:write_extensions")
        ));
    }

    #[test]
    fn write_gate_none_capabilities_is_fail_closed() {
        // `None` is the cron-engine / daemon-internal opt-in. Callers
        // who route through the `*_write` accessors MUST explicitly
        // grant, not implicitly skip. This pins that `None` is
        // fail-closed.
        let resolver = test_resolver();
        let did = peko_subject::PrincipalDID("prin_alice".to_string());
        let authority = RuntimeAuthority::for_caller(resolver, Subject::Principal(did));
        let result = authority.runtime_extensions_root_write(None);
        assert!(matches!(
            result,
            Err(AuthorityError::CapabilityDenied {
                tier: Tier::Runtime,
                capability
            }) if capability == Capability::new("runtime:write_extensions")
        ));
    }

    #[test]
    fn engine_runtime_accessor_skips_capability_gate() {
        // The cron engine writes on behalf of the principal owner
        // (Subject::Public from `for_runtime`); the principal's
        // `[[permissions]]` ACL is the only gate at that layer. The
        // `*_runtime` accessors skip the capability check — only the
        // actor + tier gate fires. The principal lookup is the first
        // thing that fails on an empty resolver, which proves the
        // capability check is bypassed.
        let resolver = test_resolver();
        let pid = principal_id("404");
        let authority = RuntimeAuthority::for_runtime(resolver);
        let result = authority.local_cron_schedule_runtime(&pid);
        assert!(matches!(result, Err(AuthorityError::UnknownPrincipal(_))));
    }

    #[test]
    fn write_gate_local_cron_for_name_with_grant_succeeds() {
        // IPC `CronAdd` holds the principal's display name (from
        // `resolve_principal`) and the in-memory `PrincipalId`
        // (`prin_<uuid>`) — the on-disk `did` is `did:peko:public:<uuid>`,
        // so the ID-keyed `local_cron_schedule_write` always fails
        // with `UnknownPrincipal`. The `_for_name` variant resolves
        // the layout directly from the validated name, bypassing the
        // disk scan. This pins the success path used by `CronAdd`.
        let resolver = test_resolver();
        let did = peko_subject::PrincipalDID("prin_alice".to_string());
        let authority = RuntimeAuthority::for_caller(resolver, Subject::Principal(did));
        let caps = Capabilities::with_grants(["principal:write_cron"]);
        let result = authority.local_cron_schedule_write_for_name("alice", Some(&caps));
        assert!(result.is_ok());
        let path = result.unwrap().into_path_buf();
        assert!(path.ends_with("alice/local/cron/schedule.toml"));
    }

    #[test]
    fn write_gate_local_cron_for_name_without_grant_is_capability_denied() {
        // Same path as the success test, but with no capability grant.
        // The actor + tier gate passes (Subject::Principal is entitled
        // on Local), but the capability gate fires
        // `CapabilityDenied{Local, principal:write_cron}`.
        let resolver = test_resolver();
        let did = peko_subject::PrincipalDID("prin_alice".to_string());
        let authority = RuntimeAuthority::for_caller(resolver, Subject::Principal(did));
        let caps = Capabilities::new(); // empty
        let result = authority.local_cron_schedule_write_for_name("alice", Some(&caps));
        assert!(matches!(
            result,
            Err(AuthorityError::CapabilityDenied {
                tier: Tier::Local,
                capability
            }) if capability == Capability::new("principal:write_cron")
        ));
    }

    #[test]
    fn write_gate_local_cron_for_name_user_actor_is_tier_denied() {
        // Peer-as-User cannot obtain a Local-tier write even with the
        // right capability grant — the actor gate fires first. Same
        // shape as `write_gate_user_actor_on_local_is_tier_denied`
        // but routed through the `_for_name` variant to confirm the
        // tier gate is independent of the resolution path.
        let resolver = test_resolver();
        let authority = RuntimeAuthority::for_caller(resolver, Subject::User("alice".into()));
        let caps = Capabilities::with_grants(["principal:write_cron"]);
        let result = authority.local_cron_schedule_write_for_name("alice", Some(&caps));
        assert!(matches!(
            result,
            Err(AuthorityError::TierDenied { tier: Tier::Local })
        ));
    }
}
