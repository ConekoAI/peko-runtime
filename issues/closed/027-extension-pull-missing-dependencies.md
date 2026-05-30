# Issue 027: Extension Pull Doesn't Auto-Install Dependencies

**Status:** Closed
**Area:** Extension System / Registry / CLI
**Related:** `src/extension/types/manifest.rs`, `src/extension/manager/mod.rs`, `src/commands/ext.rs`, `src/registry/manifest.rs`

---

## Problem

When you `peko ext pull <ref>`, it downloads and installs the extension. But if that extension declares dependencies (on other extensions or MCP servers), those dependencies are not pulled or installed automatically.

**Impact:** A pulled extension may fail to work because its dependencies are missing, with no warning or error to the user.

---

## Background

### How Dependencies Currently Work

**`ExtensionManifest`** ([manifest.rs](src/extension/types/manifest.rs)) has no dedicated `dependencies` field. Any dependency declarations must be stored in the `metadata` HashMap.

**`create_bundle`** ([manager/mod.rs:531-540](src/extension/manager/mod.rs)) reads dependencies from manifest metadata — but only when creating bundles, not during pull:

```rust
// Collect dependencies from metadata if present
if let Some(deps) = ext.manifest.get("dependencies") {
    if let Some(deps_array) = deps.as_array() {
        for dep in deps_array {
            if let Some(dep_str) = dep.as_str() {
                dependencies.push(dep_str.to_string());
            }
        }
    }
}
```

**`handle_ext_pull`** ([ext.rs:1240-1345](src/commands/ext.rs)) does a straight pull → temp file → install. It never examines the manifest for dependencies.

**`install_bundle`** ([manager/mod.rs:568-606](src/extension/manager/mod.rs)) installs extensions one-by-one but has no recursive pull logic.

**Registry layer** has `required_mcp_servers` on `RegistryManifest` ([manifest.rs:105](src/registry/manifest.rs)) — this is a protocol-level annotation for registry push/pull, stored as `dev.pekohub.requiredMcpServers` in OCI annotations. It's separate from the extension manifest's dependency declarations.

### The Gap

| Step | Expected | Actual |
|------|----------|--------|
| `peko ext pull` | Parse manifest, resolve dependencies, pull each | Just pulls one extension |
| `handle_ext_pull` | Should read `dependencies` from manifest | Never reads metadata |
| `handle_install` | Should check/install dependencies | No dependency handling |
| Registry push | Should serialize `required_mcp_servers` | Already does (protocol level) |

---

## Goal

A clean, future-proof system where:
1. Extensions declare dependencies in a structured format
2. `peko ext pull` resolves and installs dependencies automatically (or warns if it can't)
3. The dependency system supports extensions, MCP servers, and version constraints
4. Circular dependencies are detected and reported as errors

---

## Design

### 1. Add First-Class `dependencies` Field to ExtensionManifest

**`src/extension/types/manifest.rs`** — add a typed `dependencies` field:

```rust
/// A declared dependency on another extension or MCP server
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtensionDependency {
    /// Package reference (e.g., "pekohub.com/extensions/docker-skill", "mcp::filesystem")
    pub package: String,
    /// Optional version constraint (e.g., ">=1.0.0", "^2.0")
    pub version: Option<String>,
    /// Optional: marked as required vs optional
    #[serde(default)]
    pub required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtensionManifest {
    pub id: ExtensionId,
    pub extension_type: String,
    pub name: String,
    pub description: String,
    pub version: String,
    pub path: PathBuf,
    /// First-class dependency list (replaces metadata "dependencies" convention)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<ExtensionDependency>,
    /// Catch-all for forward-compatible extra fields
    #[serde(flatten)]
    pub metadata: HashMap<String, serde_json::Value>,
}
```

**Migration:** Old manifests with `metadata["dependencies"]` continue to work — read it during parse and convert to the new field. New manifests can use the structured field directly.

### 2. Add `resolve_dependencies` to ExtensionManager

**`src/extension/manager/mod.rs`** — new method on `ExtensionManager`:

```rust
/// Resolve all dependencies for an extension, checking which are already installed
/// and which need to be pulled from the registry.
pub async fn resolve_dependencies(
    &self,
    manifest: &ExtensionManifest,
) -> Result<DependencyResolution> {
    // Return: Vec of already-satisfied deps, Vec of deps needing pull
    // Detect cycles, missing required deps, version conflicts
}

/// What we know about a dependency's resolution status
pub enum DependencyStatus {
    /// Already installed and version satisfies constraint
    Satisfied,
    /// Not installed, needs pull
    Missing { package: String, required: bool },
    /// Installed but version doesn't satisfy constraint
    VersionMismatch { have: String, need: String },
}
```

### 3. Modify `handle_ext_pull` to Resolve and Pull Dependencies

**`src/commands/ext.rs`** — after pulling the main extension:

```rust
async fn handle_ext_pull(...) {
    // ... existing pull logic ...

    // NEW: After install, resolve and pull dependencies
    let manifest = /* get manifest from pulled extension */;
    let resolution = manager.resolve_dependencies(&manifest).await?;

    if !resolution.missing.is_empty() {
        if json {
            // Include dependency info in JSON output
        } else {
            println!("\nDependencies ({} need installation):", resolution.missing.len());
            for dep in &resolution.missing {
                let required_label = if dep.required { "required" } else { "optional" };
                println!("  - {} ({})", dep.package, required_label);
            }
            println!("\nPulling dependencies...");

            // Pull each missing dependency recursively
            for dep in resolution.missing {
                // Recursive call to pull dependency
                handle_ext_pull(manager, &dep.package, json, cli_registry, paths).await?;
            }
        }
    }
}
```

### 4. Optional `--no-deps` Flag forExplicit Opt-Out

Add `--no-deps` flag to `peko ext pull` for users who want to pull only the specified extension without dependency resolution.

### 5. At-Minimum Warning (If No Auto-Install)

If auto-installation is deferred or `--no-deps` is used, emit a clear warning:

```
WARNING: Extension 'docker-skill' declares 2 dependencies that are not installed:
  - required: pekohub.com/extensions/base-tools@^1.0
  - optional: pekohub.com/extensions/file-utils@>=0.5
Run 'peko ext pull --with-deps pekohub.com/extensions/docker-skill' to install them.
```

### 6. Version Constraint Support (Future-Proof)

The `ExtensionDependency.version` field uses semver-like syntax. For v1, we can treat it as informational (no enforcement), with enforcement added in a follow-up issue. This allows manifest authors to declare intent without breaking changes.

### 7. Registry Manifest: Keep `required_mcp_servers` for Protocol Level

The existing `required_mcp_servers` annotation on `RegistryManifest` ([manifest.rs:105](src/registry/manifest.rs)) should be kept — it's the **registry/protocol level** field used when pushing/pulling from the registry. It stores the same information but in a different context (wire format vs. extension manifest).

**TODO (separate issue):** Add `dependencies` field to `RegistryManifest` so it serializes to the same `dev.pekohub.requiredMcpServers` annotation, and update the backend to read/write it properly. This is out of scope for this issue.

---

## Key Design Decisions

**Why a typed `dependencies` field on ExtensionManifest, not just metadata?**
- Schema clarity: dependencies are a first-class concept, not an afterthought in a catch-all HashMap
- Validation: we can check for cycles, validate references, provide useful errors
- Discoverability: `cargo doc` and IDEs show it prominently
- Forward compatibility: structured data is easier to extend (version constraints, optional vs required, etc.)

**Why recursive pull vs. separate command?**
- User experience: `peko ext pull extension-X` just works
- Analogy: `npm install package-X` also installs its dependencies
- Users who want only the extension can use `--no-deps`

**Why allow version constraints as informational (v1)?**
- Strict version enforcement adds complexity (registry must support range queries, etc.)
- v1 can at least show the declared version to help users debug
- Enforcement can be added in a follow-up once registry supports it

**Migration path for existing manifests:**
- Old manifests store deps in `metadata["dependencies"]` (array of strings)
- New parser reads that and converts to `dependencies: Vec<ExtensionDependency>`
- This is transparent to users — no manifest rewrite required

**Scope ordering:**
1. Add `ExtensionDependency` struct and `dependencies` field to `ExtensionManifest`
2. Update manifest parsing to convert legacy metadata format
3. Add `resolve_dependencies` to `ExtensionManager`
4. Update `handle_ext_pull` to call resolve and recurse
5. Add `--no-deps` flag

---

## Tasks

- [ ] Add `ExtensionDependency` struct and `dependencies` field to `ExtensionManifest`
- [ ] Update manifest parsing (`src/extension/adapters/mod.rs`) to convert legacy `metadata["dependencies"]` to new field
- [ ] Add `resolve_dependencies` method to `ExtensionManager`
- [ ] Add `DependencyResolution` result struct with `missing` and `satisfied` lists
- [ ] Detect and report circular dependencies as errors
- [ ] Modify `handle_ext_pull` to call resolve and recursively pull dependencies
- [ ] Add `--no-deps` flag to `peko ext pull`
- [ ] Emit warning when dependencies are missing and not auto-installed
- [ ] Update JSON output to include dependency resolution info
- [ ] Test: pull extension with no dependencies (existing behavior unchanged)
- [ ] Test: pull extension with satisfied dependencies (installs, no extra pull)
- [ ] Test: pull extension with missing required dependencies (recursively pulls each)
- [ ] Test: pull with `--no-deps` skips dependency resolution
- [ ] Test: circular dependency detection reports error
- [ ] Test: legacy manifest with `metadata["dependencies"]` still works

---

## Background Context

This issue was spawned from a real user scenario: pulling an extension from the registry only to find it doesn't work because the extension expected an MCP server or base extension to already be present. The extension manifest declared those requirements nowhere that `peko ext pull` would read.

The fix is analogous to how `npm install` handles package dependencies — the install command handles the whole dependency tree, not just the root package.