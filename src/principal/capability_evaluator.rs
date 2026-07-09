//! Capability evaluator
//!
//! Given a Principal's capability grants and an extension's declared
//! `provides` / `requires`, the evaluator decides whether the extension is
//! active and whether a specific capability is usable.
//!
//! This is intentionally lightweight: it does not own the extension store or
//! the Principal config. Callers pass those in, keeping the evaluator easy to
//! unit test and cheap to instantiate per evaluation.

use crate::extensions::framework::types::ExtensionManifest;
use crate::principal::{Capabilities, Capability};

/// Evaluates capability grants against extension manifests.
#[derive(Debug, Clone, Copy, Default)]
pub struct CapabilityEvaluator;

impl CapabilityEvaluator {
    /// Create a new evaluator.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Whether `grants` satisfy a single required capability.
    #[must_use]
    pub fn is_granted(grants: &Capabilities, required: &str) -> bool {
        grants.is_granted(&Capability::new(required))
    }

    /// Whether an extension is active under the given grants.
    ///
    /// Rules:
    /// - The extension must be detected/installed (the caller decides that).
    /// - At least one provided capability must be granted, unless the
    ///   extension declares no `provides`, in which case it is considered
    ///   implicitly provided by its own id (as `tool:<id>` or the kind
    ///   prefix supplied by the caller).
    /// - All `requires` capabilities must be satisfied.
    #[must_use]
    pub fn is_extension_active(
        &self,
        manifest: &ExtensionManifest,
        grants: &Capabilities,
        implicit_kind: Option<&str>,
    ) -> bool {
        // All requirements must be satisfied.
        if manifest
            .requires
            .iter()
            .any(|req| !Self::is_granted(grants, req))
        {
            return false;
        }

        // If the extension declares nothing it provides, fall back to its id.
        if manifest.provides.is_empty() {
            let implicit = match implicit_kind {
                Some(kind) => format!("{kind}:{}", manifest.id.0),
                None => format!("tool:{}", manifest.id.0),
            };
            return Self::is_granted(grants, &implicit);
        }

        // At least one provided capability must be granted.
        manifest
            .provides
            .iter()
            .any(|provided| Self::is_granted(grants, provided))
    }

    /// Return every capability from the extension that is currently granted.
    #[must_use]
    pub fn active_provides(
        &self,
        manifest: &ExtensionManifest,
        grants: &Capabilities,
    ) -> Vec<String> {
        manifest
            .provides
            .iter()
            .filter(|provided| Self::is_granted(grants, provided))
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn manifest_with(id: &str, provides: &[&str], requires: &[&str]) -> ExtensionManifest {
        ExtensionManifest {
            id: crate::extensions::framework::types::ExtensionId::new(id),
            extension_type: "tool".to_string(),
            name: id.to_string(),
            description: "test".to_string(),
            version: "1.0.0".to_string(),
            path: PathBuf::from("/tmp"),
            dependencies: Vec::new(),
            provides: provides.iter().map(|s| s.to_string()).collect(),
            requires: requires.iter().map(|s| s.to_string()).collect(),
            metadata: std::collections::HashMap::new(),
            source: None,
        }
    }

    #[test]
    fn active_when_provided_capability_granted() {
        let manifest = manifest_with("docker-skill", &["skill:docker"], &[]);
        let grants = Capabilities::with_grants(["skill:docker"]);

        assert!(CapabilityEvaluator::new().is_extension_active(&manifest, &grants, None));
    }

    #[test]
    fn inactive_when_no_provided_capability_granted() {
        let manifest = manifest_with("docker-skill", &["skill:docker"], &[]);
        let grants = Capabilities::with_grants(["skill:kubernetes"]);

        assert!(!CapabilityEvaluator::new().is_extension_active(&manifest, &grants, None));
    }

    #[test]
    fn inactive_when_requirement_missing() {
        let manifest = manifest_with("docker-skill", &["skill:docker"], &["tool:Read"]);
        let grants = Capabilities::with_grants(["skill:docker"]);

        assert!(!CapabilityEvaluator::new().is_extension_active(&manifest, &grants, None));
    }

    #[test]
    fn active_when_all_requirements_satisfied() {
        let manifest = manifest_with("docker-skill", &["skill:docker"], &["tool:Read", "network"]);
        let grants = Capabilities::with_grants(["skill:docker", "tool:Read", "network"]);

        assert!(CapabilityEvaluator::new().is_extension_active(&manifest, &grants, None));
    }

    #[test]
    fn wildcard_grant_satisfies_requirement() {
        let manifest = manifest_with("docker-skill", &["skill:docker"], &["tool:Read"]);
        let grants = Capabilities::with_grants(["skill:docker", "tool:*"]);

        assert!(CapabilityEvaluator::new().is_extension_active(&manifest, &grants, None));
    }

    #[test]
    fn implicit_provided_capability_uses_kind() {
        let manifest = manifest_with("researcher", &[], &[]);
        let grants = Capabilities::with_grants(["agent:researcher"]);

        assert!(CapabilityEvaluator::new().is_extension_active(
            &manifest,
            &grants,
            Some("agent")
        ));
    }

    #[test]
    fn implicit_provided_capability_defaults_to_tool() {
        let manifest = manifest_with("custom-tool", &[], &[]);
        let grants = Capabilities::with_grants(["tool:custom-tool"]);

        assert!(
            CapabilityEvaluator::new().is_extension_active(&manifest, &grants, None)
        );
    }

    #[test]
    fn active_provides_returns_only_granted() {
        let manifest = manifest_with(
            "agency",
            &["agent:researcher", "agent:writer", "skill:briefing"],
            &[],
        );
        let grants = Capabilities::with_grants(["agent:*"]);
        let active = CapabilityEvaluator::new().active_provides(&manifest, &grants);

        assert!(active.contains(&"agent:researcher".to_string()));
        assert!(active.contains(&"agent:writer".to_string()));
        assert!(!active.contains(&"skill:briefing".to_string()));
    }
}
