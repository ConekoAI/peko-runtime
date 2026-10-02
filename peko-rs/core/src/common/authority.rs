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
//! ADR-066 D9: the writer-side grant check (`principal:write_*`
//! capability strings) is replaced by plain **ownership comparison** —
//! a principal-typed actor may write only its own principal's tiers; a
//! crossing attempt fails closed and emits a `Security`-severity audit
//! event. Operator actors (`Subject::User`) and the runtime itself
//! (`Subject::Public`, via the `*_runtime` accessors) are trusted.
//! See the `*_write` family and the engine-internal `*_runtime`
//! accessors below.
//!
//! See [`TierPath`] for the tier-typed wrapper API.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use peko_subject::{PrincipalId, Subject};

use crate::common::paths::{PathResolver, RuntimeLayout};

// 2026-08-25: `principal:write_cron` was retired along with the
// `CronList` / `CronAdd` / `CronRemove` / `CronRun` / `CronHistory`
// IPC variants and the `peko cron` CLI. Cron is now an internal
// principal tool. ADR-066 P2 deleted the remaining `principal:write_*`
// / `runtime:write_*` grant strings — writes gate on ownership
// (see the `*_write` family below).

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
    /// Audit sink for `Security`-severity events on crossing-write
    /// denials (ADR-066 D9). `None` (tests, bare fixture builders)
    /// skips the event — the denial still fails closed.
    audit: Option<Arc<peko_observability::Observability>>,
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

    /// A principal-typed actor (or visitor) attempted to write another
    /// principal's tier. ADR-066 D9: writes gate on ownership — whose
    /// files these are — not on grant strings. The denial is also
    /// emitted as a `Security` audit event when the authority carries
    /// an audit sink.
    #[error("cross-principal write denied: {actor} may not write principal '{principal}'")]
    OwnershipDenied { actor: String, principal: String },
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
            audit: None,
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
            audit: None,
        }
    }

    /// Attach the observability hub so ownership-crossing denials emit
    /// a `Security`-severity audit event (ADR-066 D9).
    #[must_use]
    pub fn with_audit_sink(
        mut self,
        audit: Option<Arc<peko_observability::Observability>>,
    ) -> Self {
        self.audit = audit;
        self
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
        let (_name, layout) = self.principal_layout(principal)?;
        Ok(LocalPath(layout.local.root))
    }

    /// Hand out a `LocalPath` for the principal's cron schedule file.
    pub fn local_cron_schedule(
        &self,
        principal: &PrincipalId,
    ) -> Result<LocalPath, AuthorityError> {
        self.assert_local_entitled()?;
        let (_name, layout) = self.principal_layout(principal)?;
        Ok(LocalPath(layout.local.cron_schedule))
    }

    /// Hand out a `LocalPath` for the principal's sessions directory.
    pub fn local_sessions_dir(&self, principal: &PrincipalId) -> Result<LocalPath, AuthorityError> {
        self.assert_local_entitled()?;
        let (_name, layout) = self.principal_layout(principal)?;
        Ok(LocalPath(layout.local.sessions_dir))
    }

    // ---------------------------------------------------------------------
    // Shared tier — per-principal capability-bearing config (principal
    // identity, agents, MCP configs, the kb knowledge base). Any
    // authenticated actor with visibility on the principal can read;
    // writes require the principal owner.
    //
    // The read gate is permissive — any non-Public actor can ask for
    // Shared paths and we'll hand them out. The write side gates on
    // ownership (ADR-066 D9 — see the `*_write` family below).
    // ---------------------------------------------------------------------

    /// Hand out a `SharedPath` for `principal.toml`.
    pub fn shared_config(&self, principal: &PrincipalId) -> Result<SharedPath, AuthorityError> {
        self.assert_shared_read_entitled()?;
        let (_name, layout) = self.principal_layout(principal)?;
        Ok(SharedPath(layout.shared.config_file))
    }

    /// Hand out a `SharedPath` for the principal's agents directory.
    pub fn shared_roles_dir(&self, principal: &PrincipalId) -> Result<SharedPath, AuthorityError> {
        self.assert_shared_read_entitled()?;
        let (_name, layout) = self.principal_layout(principal)?;
        Ok(SharedPath(layout.shared.roles_dir))
    }

    /// Hand out a `SharedPath` for the principal's identity directory
    /// (`identity.json`).
    pub fn shared_identity_dir(
        &self,
        principal: &PrincipalId,
    ) -> Result<SharedPath, AuthorityError> {
        self.assert_shared_read_entitled()?;
        let (_name, layout) = self.principal_layout(principal)?;
        // The identity dir is the Shared root joined with `"identity"`,
        // matching the legacy `principal_identity_dir` layout.
        Ok(SharedPath(layout.shared.root.join("identity")))
    }

    /// Hand out a `SharedPath` for the principal's MCP server configs.
    pub fn shared_mcps_dir(&self, principal: &PrincipalId) -> Result<SharedPath, AuthorityError> {
        self.assert_shared_read_entitled()?;
        let (_name, layout) = self.principal_layout(principal)?;
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
    // WriteSide ownership gate (ADR-066 D9).
    //
    // The reader-side actor gate above decides whether an actor MAY touch
    // a tier at all. The writer-side gate decides whether they may WRITE:
    // plain ownership comparison — an operator (`Subject::User`) writes
    // any principal's tiers; a principal-typed actor writes only its own;
    // visitors never write. A crossing attempt fails closed and emits a
    // `Security` audit event (when the authority carries a sink). No grant
    // language remains: the boundary is whose files these are.
    // ---------------------------------------------------------------------

    /// Hand out a `SharedPath` for `principal.toml` IF the actor owns
    /// the principal (or is the operator). Tier gate fires first.
    pub async fn shared_config_write(
        &self,
        principal: &PrincipalId,
    ) -> Result<SharedPath, AuthorityError> {
        self.assert_shared_read_entitled()?;
        let (name, layout) = self.principal_layout(principal)?;
        self.assert_write_owner_for_id(&name, principal, Tier::Shared)
            .await?;
        Ok(SharedPath(layout.shared.config_file))
    }

    /// Name-keyed variant of [`shared_config_write`] for IPC
    /// `PrincipalGrantPermission` / `PrincipalSetStatus` /
    /// `PrincipalSetExposure` / `PrincipalUpdate`, where the
    /// caller's `PrincipalId` may not round-trip through
    /// `lookup_principal_name` (e.g. the principal's on-disk
    /// `did = None` because it was created via the CLI default).
    /// The actor + ownership gate is identical; the layout is
    /// resolved directly from the validated name.
    pub async fn shared_config_write_for_name(
        &self,
        principal_name: &str,
    ) -> Result<SharedPath, AuthorityError> {
        self.assert_shared_read_entitled()?;
        let layout = self.resolver.principal_layout(principal_name);
        self.assert_write_owner_for_name(principal_name, Tier::Shared)
            .await?;
        Ok(SharedPath(layout.shared.config_file))
    }

    /// Hand out a `SharedPath` for the agents directory IF the actor
    /// owns the principal. Gates `agents/` + the
    /// `agents/primary.md` write done by `PrincipalCreate`.
    pub async fn shared_roles_dir_write(
        &self,
        principal: &PrincipalId,
    ) -> Result<SharedPath, AuthorityError> {
        self.assert_shared_read_entitled()?;
        let (name, layout) = self.principal_layout(principal)?;
        self.assert_write_owner_for_id(&name, principal, Tier::Shared)
            .await?;
        Ok(SharedPath(layout.shared.roles_dir))
    }

    /// Name-keyed variant of [`shared_roles_dir_write`] for
    /// `PrincipalCreate`, where the principal's `PrincipalId` has
    /// not yet been generated (`PrincipalManager::create` assigns
    /// it). The actor + ownership gate is identical; the layout is
    /// resolved directly from the validated name.
    pub async fn shared_roles_dir_write_for_name(
        &self,
        principal_name: &str,
    ) -> Result<SharedPath, AuthorityError> {
        self.assert_shared_read_entitled()?;
        let layout = self.resolver.principal_layout(principal_name);
        self.assert_write_owner_for_name(principal_name, Tier::Shared)
            .await?;
        Ok(SharedPath(layout.shared.roles_dir))
    }

    /// Hand out a `SharedPath` for the identity directory IF the actor
    /// owns the principal.
    pub async fn shared_identity_dir_write(
        &self,
        principal: &PrincipalId,
    ) -> Result<SharedPath, AuthorityError> {
        self.assert_shared_read_entitled()?;
        let (name, layout) = self.principal_layout(principal)?;
        self.assert_write_owner_for_id(&name, principal, Tier::Shared)
            .await?;
        Ok(SharedPath(layout.shared.root.join("identity")))
    }

    /// Name-keyed variant of [`shared_identity_dir_write`] for
    /// `principal_unpackager::import_identity`, where the new
    /// principal's `PrincipalId` has not yet been generated by the
    /// manager. The actor + ownership gate is identical; the layout
    /// is resolved directly from the validated name.
    pub async fn shared_identity_dir_write_for_name(
        &self,
        principal_name: &str,
    ) -> Result<SharedPath, AuthorityError> {
        self.assert_shared_read_entitled()?;
        let layout = self.resolver.principal_layout(principal_name);
        self.assert_write_owner_for_name(principal_name, Tier::Shared)
            .await?;
        Ok(SharedPath(layout.shared.root.join("identity")))
    }

    /// Hand out a `SharedPath` for the MCP server configs IF the actor
    /// owns the principal.
    pub async fn shared_mcps_dir_write(
        &self,
        principal: &PrincipalId,
    ) -> Result<SharedPath, AuthorityError> {
        self.assert_shared_read_entitled()?;
        let (name, layout) = self.principal_layout(principal)?;
        self.assert_write_owner_for_id(&name, principal, Tier::Shared)
            .await?;
        Ok(SharedPath(layout.shared.mcps_dir))
    }

    /// Hand out a `RuntimePath` for the extensions install root.
    /// Runtime-tier writes belong to the runtime / operator — a
    /// principal-typed or visitor actor is denied (and audited).
    pub async fn runtime_extensions_root_write(&self) -> Result<RuntimePath, AuthorityError> {
        match self.actor.as_ref() {
            Subject::Public | Subject::User(_) => {}
            Subject::Principal(_) | Subject::Visitor(_) => {
                self.deny_crossing(self.actor_label(), "runtime:extensions", Tier::Runtime)
                    .await?;
            }
        }
        Ok(RuntimePath(self.resolver.extensions_root()))
    }
    // ---------------------------------------------------------------------
    // Engine-internal `_runtime` accessors.
    //
    // The cron engine writes cron files on behalf of the principal owner
    // (not on behalf of a peer). The principal's `[[permissions]]` ACL
    // is the only gate at that layer — there is no per-grant capability
    // check because the engine isn't a peer session. Use these methods
    // from the cron engine. The legacy `local_cron_schedule_write*`
    // IPC accessors were deleted in the cron-as-internal-tool refactor
    // (2026-08-25): cron is now a fully internal principal tool gated
    // by `tool:Cron{Create,List,Delete}` grants in the agentic-loop
    // funnel (F37). The IPC cron variants are gone.
    // ---------------------------------------------------------------------

    /// **Cron-engine-only.** Hands out a `LocalPath` for the principal's
    /// cron schedule file, bypassing the ownership gate (the engine
    /// writes on behalf of the principal; the principal's `[[permissions]]`
    /// ACL is the only gate). The legacy IPC `*_write` accessors were
    /// deleted in the cron-as-internal-tool refactor (2026-08-25):
    /// cron is now a fully internal principal tool.
    pub fn local_cron_schedule_runtime(
        &self,
        principal: &PrincipalId,
    ) -> Result<LocalPath, AuthorityError> {
        self.assert_local_entitled()?;
        let (_name, layout) = self.principal_layout(principal)?;
        Ok(LocalPath(layout.local.cron_schedule))
    }

    /// **Cron-engine-only.** Like [`local_cron_schedule_runtime`] but
    /// accepts a pre-resolved principal name (the result of the
    /// manager-aware `principal_name_for` lookup). The cron engine uses
    /// this when it has already resolved DID → name through
    /// `PrincipalManager::get` and doesn't want a second disk scan.
    /// The legacy IPC `*_write` accessors were deleted in the
    /// cron-as-internal-tool refactor (2026-08-25).
    pub fn local_cron_schedule_runtime_for_name(
        &self,
        principal_name: &str,
    ) -> Result<LocalPath, AuthorityError> {
        self.assert_local_entitled()?;
        let layout = self.resolver.principal_layout(principal_name);
        Ok(LocalPath(layout.local.cron_schedule))
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
    ) -> Result<(String, crate::common::paths::PrincipalLayout), AuthorityError> {
        let name = self
            .resolver
            .lookup_principal_name(principal)
            .ok_or_else(|| AuthorityError::UnknownPrincipal(principal.clone()))?;
        Ok((name.clone(), self.resolver.principal_layout(&name)))
    }

    /// Local-tier gate: only the runtime (`Subject::Public`) or a
    /// principal-typed subject may receive a `LocalPath`. Peer-as-User
    /// (CLI without a peer session) cannot obtain a `LocalPath`;
    /// neither can a hub-minted visitor (ADR-058 D5 — visitors are
    /// ordinary non-owner peers with no local authority).
    fn assert_local_entitled(&self) -> Result<(), AuthorityError> {
        match self.actor.as_ref() {
            Subject::Public | Subject::Principal(_) => Ok(()),
            Subject::User(_) | Subject::Visitor(_) => {
                Err(AuthorityError::TierDenied { tier: Tier::Local })
            }
        }
    }

    /// Shared-tier read gate (permissive). Any non-`Public` actor can
    /// read Shared paths; the write side gates on ownership (ADR-066
    /// D9). Visitors count as non-`Public` here: they are hub-minted
    /// identified peers, not unauthenticated access.
    fn assert_shared_read_entitled(&self) -> Result<(), AuthorityError> {
        match self.actor.as_ref() {
            Subject::Public => Err(AuthorityError::TierDenied { tier: Tier::Shared }),
            Subject::User(_) | Subject::Principal(_) | Subject::Visitor(_) => Ok(()),
        }
    }

    /// Write-side ownership gate for an ID-keyed target whose name the
    /// caller already resolved via `principal_layout` (ADR-066 D9).
    /// Ownership compares the actor's DID against the target's on-disk
    /// `did` — robust to both `PrincipalId` string forms (`prin_*` and
    /// the bare DID).
    async fn assert_write_owner_for_id(
        &self,
        principal_name: &str,
        principal: &PrincipalId,
        tier: Tier,
    ) -> Result<(), AuthorityError> {
        match self.actor.as_ref() {
            // The operator (local CLI / authenticated hub user) writes
            // any principal's tiers.
            Subject::User(_) => Ok(()),
            // A principal-typed actor writes only its own tiers. The
            // target's DID comes from its on-disk config, not from the
            // caller-supplied id string.
            Subject::Principal(did) => {
                match self.principal_did_for_name(principal_name).as_deref() {
                    Some(target) if target == did.0 => Ok(()),
                    _ => {
                        self.deny_crossing(self.actor_label(), principal_name, tier)
                            .await
                    }
                }
            }
            Subject::Visitor(_) | Subject::Public => {
                self.deny_crossing(self.actor_label(), &principal.0, tier)
                    .await
            }
        }
    }

    /// Write-side ownership gate for a name-keyed target. Resolves the
    /// target principal's DID from its on-disk config; a target that
    /// doesn't exist yet (PrincipalCreate) is writable by the operator
    /// only — a principal-typed actor can never prove ownership of a
    /// principal that isn't on disk, so it fails closed.
    async fn assert_write_owner_for_name(
        &self,
        principal_name: &str,
        tier: Tier,
    ) -> Result<(), AuthorityError> {
        match self.actor.as_ref() {
            Subject::User(_) => Ok(()),
            Subject::Principal(did) => {
                match self.principal_did_for_name(principal_name).as_deref() {
                    Some(target) if target == did.0 => Ok(()),
                    _ => {
                        self.deny_crossing(self.actor_label(), principal_name, tier)
                            .await
                    }
                }
            }
            Subject::Visitor(_) | Subject::Public => {
                self.deny_crossing(self.actor_label(), principal_name, tier)
                    .await
            }
        }
    }

    /// Read a principal's DID from its on-disk `principal.toml`.
    /// `None` when the principal isn't on disk or carries no `did`.
    fn principal_did_for_name(&self, principal_name: &str) -> Option<String> {
        let config_path = self
            .resolver
            .principal_layout(principal_name)
            .shared
            .config_file;
        let contents = std::fs::read_to_string(config_path).ok()?;
        let value: toml::Value = contents.parse().ok()?;
        value
            .get("did")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .filter(|d| !d.is_empty())
    }

    /// Wire form of the actor for error messages + audit details.
    fn actor_label(&self) -> String {
        self.actor.to_string()
    }

    /// Fail closed and emit the ADR-066 D9 `Security` audit event when
    /// the authority carries an audit sink.
    async fn deny_crossing(
        &self,
        actor: String,
        target: &str,
        tier: Tier,
    ) -> Result<(), AuthorityError> {
        if let Some(audit) = self.audit.as_ref() {
            let result = audit
                .audit_security_with_caller(
                    Some(self.actor.as_ref()),
                    "principal.cross_principal_write_denied",
                    None,
                    serde_json::json!({
                        "target_principal": target,
                        "path_tier": format!("{tier:?}").to_lowercase(),
                    }),
                )
                .await;
            if let Err(e) = result {
                tracing::warn!("failed to emit cross-principal-write audit event: {e}");
            }
        }
        Err(AuthorityError::OwnershipDenied {
            actor,
            principal: target.to_string(),
        })
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
    // ADR-066 D9 — ownership write-gate tests.
    //
    // The reader-side actor gate fires first; the writer-side ownership
    // gate stacks on top. Together they pin that:
    // (a) a principal-typed actor writes its own tiers;
    // (b) a principal-typed actor writing another principal's tiers
    //     gets `OwnershipDenied` (not `TierDenied`) and a Security
    //     audit event;
    // (c) the operator (`Subject::User`) writes any principal;
    // (d) a `Subject::Public` actor still gets `TierDenied` on Shared
    //     (the ownership gate cannot rescue a tier-denied actor);
    // (e) a `Subject::User` actor still gets `TierDenied` on Local;
    // (f) visitors never write.
    // ---------------------------------------------------------------------

    /// Build a tempdir-backed `PathResolver` with a real
    /// `principals/alice/principal.toml` on disk. The on-disk `did`
    /// matches the returned `PrincipalId` so `lookup_principal_name`
    /// succeeds. Returns `(TempDir, PathResolver, PrincipalId)` —
    /// caller keeps the tempdir alive for the duration of the test.
    fn with_real_principal() -> (tempfile::TempDir, PathResolver, PrincipalId) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_dir = tmp.path().join("config");
        let data_dir = tmp.path().join("data");
        let cache_dir = tmp.path().join("cache");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::create_dir_all(&cache_dir).unwrap();

        // On-disk DID format mirrors what `peko_identity` produces;
        // ID-keyed accessors scan for this string verbatim.
        let did_str = "did:peko:public:alice-test-fixture";
        let principal_dir = config_dir.join("principals").join("alice");
        std::fs::create_dir_all(&principal_dir).unwrap();
        std::fs::write(
            principal_dir.join("principal.toml"),
            format!("name = \"alice\"\ndid = \"{did_str}\"\n"),
        )
        .unwrap();

        let resolver = PathResolver::with_dirs(config_dir, data_dir, cache_dir);
        let pid = PrincipalId(did_str.to_string());
        (tmp, resolver, pid)
    }

    /// The fixture principal's DID (kept in one place so the actor
    /// construction can't drift from the on-disk value).
    fn alice_did() -> peko_subject::PrincipalDID {
        peko_subject::PrincipalDID("did:peko:public:alice-test-fixture".to_string())
    }

    // ----- ID-keyed accessors --------------------------------------------

    #[tokio::test]
    async fn owner_writes_own_shared_tiers() {
        let (_tmp, resolver, pid) = with_real_principal();
        let authority = RuntimeAuthority::for_caller(resolver, Subject::Principal(alice_did()));
        assert!(authority.shared_config_write(&pid).await.is_ok());
        assert!(authority.shared_roles_dir_write(&pid).await.is_ok());
        assert!(authority.shared_identity_dir_write(&pid).await.is_ok());
        assert!(authority.shared_mcps_dir_write(&pid).await.is_ok());
    }

    #[tokio::test]
    async fn cross_principal_write_is_denied() {
        let (_tmp, resolver, pid) = with_real_principal();
        let mallory = peko_subject::PrincipalDID("did:peko:public:mallory".to_string());
        let authority = RuntimeAuthority::for_caller(resolver, Subject::Principal(mallory));
        let result = authority.shared_config_write(&pid).await;
        assert!(
            matches!(result, Err(AuthorityError::OwnershipDenied { .. })),
            "crossing write must fail closed, got: {result:?}"
        );
    }

    #[tokio::test]
    async fn cross_principal_write_emits_security_audit_event() {
        let (_tmp, resolver, pid) = with_real_principal();
        let audit_dir = _tmp.path().join("audit");
        let observability = Arc::new(
            peko_observability::Observability::with_audit_dir("test", audit_dir.clone())
                .expect("audit dir"),
        );
        let mallory = peko_subject::PrincipalDID("did:peko:public:mallory".to_string());
        let authority = RuntimeAuthority::for_caller(resolver, Subject::Principal(mallory))
            .with_audit_sink(Some(observability));

        let result = authority.shared_roles_dir_write(&pid).await;
        assert!(matches!(
            result,
            Err(AuthorityError::OwnershipDenied { .. })
        ));

        // The durable JSONL sink writes within the awaited call — read
        // today's file back and pin the event.
        let today = chrono::Utc::now().date_naive();
        let log = std::fs::read_to_string(audit_dir.join(format!("audit-{today}.jsonl")))
            .expect("audit file written");
        assert!(
            log.contains("principal.cross_principal_write_denied"),
            "audit log must carry the crossing event: {log}"
        );
        assert!(
            log.contains("\"severity\":\"security\""),
            "event must be Security severity: {log}"
        );
    }

    #[tokio::test]
    async fn operator_writes_any_principal() {
        let (_tmp, resolver, pid) = with_real_principal();
        let authority = RuntimeAuthority::for_caller(resolver, Subject::User("local".into()));
        assert!(authority.shared_config_write(&pid).await.is_ok());
    }

    #[tokio::test]
    async fn visitor_write_is_denied() {
        let (_tmp, resolver, pid) = with_real_principal();
        let authority =
            RuntimeAuthority::for_caller(resolver, Subject::Visitor("visitor-1".to_string()));
        let result = authority.shared_config_write(&pid).await;
        assert!(matches!(
            result,
            Err(AuthorityError::OwnershipDenied { .. })
        ));
    }

    #[tokio::test]
    async fn public_actor_on_shared_write_is_tier_denied() {
        // The runtime (Subject::Public) cannot read Shared-tier paths
        // because it should always go through the Local runtime paths
        // when operating on its own behalf. The ownership gate cannot
        // rescue a tier-denied actor.
        let resolver = test_resolver();
        let authority = RuntimeAuthority::for_runtime(resolver);
        let pid = principal_id("alice");
        let result = authority.shared_config_write(&pid).await;
        assert!(matches!(
            result,
            Err(AuthorityError::TierDenied { tier: Tier::Shared })
        ));
    }

    // ----- _for_name accessors --------------------------------------------

    #[tokio::test]
    async fn owner_writes_own_shared_tiers_for_name() {
        let (_tmp, resolver, _pid) = with_real_principal();
        let authority = RuntimeAuthority::for_caller(resolver, Subject::Principal(alice_did()));
        let result = authority.shared_config_write_for_name("alice").await;
        assert!(result.is_ok());
        assert!(result.unwrap().as_path().ends_with("alice/principal.toml"));
        assert!(authority
            .shared_roles_dir_write_for_name("alice")
            .await
            .is_ok());
        assert!(authority
            .shared_identity_dir_write_for_name("alice")
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn cross_principal_write_for_name_is_denied() {
        let (_tmp, resolver, _pid) = with_real_principal();
        let mallory = peko_subject::PrincipalDID("did:peko:public:mallory".to_string());
        let authority = RuntimeAuthority::for_caller(resolver, Subject::Principal(mallory));
        let result = authority.shared_roles_dir_write_for_name("alice").await;
        assert!(matches!(
            result,
            Err(AuthorityError::OwnershipDenied { .. })
        ));
    }

    /// A principal-typed actor can never prove ownership of a principal
    /// that isn't on disk — fail closed. (The operator creates new
    /// principals; `Subject::User` is unaffected.)
    #[tokio::test]
    async fn principal_actor_write_for_unknown_name_is_denied() {
        let resolver = test_resolver();
        let authority = RuntimeAuthority::for_caller(resolver, Subject::Principal(alice_did()));
        let result = authority.shared_roles_dir_write_for_name("ghost").await;
        assert!(matches!(
            result,
            Err(AuthorityError::OwnershipDenied { .. })
        ));
    }

    /// The operator's `_for_name` path doesn't require the principal to
    /// exist on disk (PrincipalCreate writes the first files before the
    /// manager assigns the id).
    #[tokio::test]
    async fn operator_write_for_unknown_name_yields_path() {
        let resolver = test_resolver();
        let authority = RuntimeAuthority::for_caller(resolver, Subject::User("local".into()));
        let result = authority.shared_roles_dir_write_for_name("ghost").await;
        assert!(result.is_ok());
        let path = result.unwrap().into_path_buf();
        assert!(path.ends_with("ghost/roles"));
    }

    // ----- Runtime tier ----------------------------------------------------

    #[tokio::test]
    async fn runtime_tier_write_allows_operator_and_runtime() {
        let resolver = test_resolver();
        let as_operator =
            RuntimeAuthority::for_caller(resolver.clone(), Subject::User("local".into()));
        assert!(as_operator.runtime_extensions_root_write().await.is_ok());
        let as_runtime = RuntimeAuthority::for_runtime(resolver);
        assert!(as_runtime.runtime_extensions_root_write().await.is_ok());
    }

    #[tokio::test]
    async fn runtime_tier_write_denies_principal_actor() {
        let resolver = test_resolver();
        let authority = RuntimeAuthority::for_caller(resolver, Subject::Principal(alice_did()));
        let result = authority.runtime_extensions_root_write().await;
        assert!(matches!(
            result,
            Err(AuthorityError::OwnershipDenied { .. })
        ));
    }

    #[tokio::test]
    async fn engine_runtime_accessor_skips_ownership_gate() {
        // The cron engine writes on behalf of the principal owner
        // (Subject::Public from `for_runtime`); the principal's
        // `[[permissions]]` ACL is the only gate at that layer. The
        // `*_runtime` accessors skip the ownership check — only the
        // actor + tier gate fires. The principal lookup is the first
        // thing that fails on an empty resolver, which proves the
        // ownership check is bypassed.
        let resolver = test_resolver();
        let pid = principal_id("404");
        let authority = RuntimeAuthority::for_runtime(resolver);
        let result = authority.local_cron_schedule_runtime(&pid);
        assert!(matches!(result, Err(AuthorityError::UnknownPrincipal(_))));
    }
}
