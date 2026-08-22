#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""Workspace dependency-graph check.

Phase 12b of the Cargo workspace migration. Enforces the forbidden
crate-to-crate edges from the workspace-migration plan, replacing
the path-grep approach in ``check_module_boundaries.sh`` with a
deterministic dependency-graph read of every ``peko-rs/*/Cargo.toml``
plus the root crate.

The script:
1. Walks ``peko-rs/*/Cargo.toml`` (one workspace member each) and
   the root ``Cargo.toml``.
2. Extracts ``[dependencies]``, ``[dev-dependencies]``, and
   ``[build-dependencies]`` sections for each member.
3. Filters for ``peko-*`` workspace-internal dependencies (the only
   edges we police; external crates go through ``Cargo.lock``).
4. Builds the directed edge graph (A → B iff A depends on B).
5. Asserts the forbidden-edge table below.
6. Prints the full graph if ``--print-graph`` is passed.

Forbidden-edge table (from the workspace-migration plan + per-phase
PR descriptions):

  - ``peko-provider-api`` MUST NOT depend on ``peko-engine``
    (Rule 11 from Phase 1, "providers→engine ban").
  - ``peko-engine`` MUST NOT depend on ``peko-protocol`` or
    ``peko-peko-daemon``.
  - ``peko-protocol`` MUST NOT depend on any other ``peko-*``
    crate (wire-only contract; ``serde`` + ``serde_json`` only).
  - ``peko-subject``, ``peko-message``, ``peko-tools-core``,
    ``peko-events`` MUST NOT depend on any other ``peko-*`` crate
    (pure value/type layers).
  - ``peko-quota`` MAY depend only on ``peko-message``.
  - ``peko-extension-api`` MUST NOT depend on ``peko-extension-host``
    or any implementation crate (``peko-engine``, ``peko-protocol``).

These are the documented plan rules. The script reports any new
forbidden edge the moment it appears in a ``Cargo.toml``, before a
PR can land. Pure path-based ``check_module_boundaries.sh`` catches
the old in-``src/`` rules; this script catches the new
crate-level rules.

Exit codes:
  0 — clean
  1 — forbidden edge detected (or parse error)
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path
from typing import Dict, List, Set, Tuple

# ---------------------------------------------------------------------------
# Forbidden-edge table
# ---------------------------------------------------------------------------
#
# Each entry is (from_crate, to_crate, reason). The script asserts that
# no edge in the actual graph appears in this table. Edge directions
# are strict: ``A MUST NOT → B`` means A's Cargo.toml must not list B
# under any of ``[dependencies]`` / ``[dev-dependencies]`` /
# ``[build-dependencies]``.
#
# Adding a new entry here is the *only* way to add a rule — the script
# will surface every Cargo.toml dependency that violates it.

FORBIDDEN_EDGES: List[Tuple[str, str, str]] = [
    # Rule 11 — providers→engine ban (Phase 1)
    (
        "peko-provider-api",
        "peko-engine",
        "providers must depend on provider-api/contracts only — engine imports "
        "would invert the contract and reintroduce the F6/F7 abstraction slip.",
    ),
    # Phase 6 — concrete peko-providers must not depend on peko-engine.
    # `peko-engine` consumes `peko_providers::ProviderView` as a
    # back-compat re-export, but the direction engine→providers is the
    # only allowed one. Inverting it would drag the agentic loop,
    # compaction, and tool funnel into the providers crate.
    (
        "peko-providers",
        "peko-engine",
        "concrete providers must depend on provider-api/message/quota/events/subject "
        "only — engine imports would invert the contract and force the "
        "agentic loop, compaction, and tool funnel to live next to the providers.",
    ),
    # peko-engine must not depend on the IPC protocol crate
    (
        "peko-engine",
        "peko-protocol",
        "engine runs in-process; CLI/daemon framing belongs to peko-runtime/peko-cli.",
    ),
    # peko-engine must not depend on the peko-daemon binary crate
    (
        "peko-engine",
        "peko-peko-daemon",
        "engine is library code; the daemon binary is a separate entry point.",
    ),
    # Phase 0.Z-B: peko-engine must not depend on the peko-cli binary crate.
    (
        "peko-engine",
        "peko-peko-cli",
        "engine is library code; the CLI binary is a separate entry point.",
    ),
    # peko-protocol is a wire-only contract
    (
        "peko-protocol",
        "peko-message",
        "protocol is serde+serde_json only; lifting message types would "
        "break the 'CLI and daemon meet only here' invariant.",
    ),
    (
        "peko-protocol",
        "peko-tools-core",
        "protocol is serde+serde_json only; tool API is irrelevant to the wire.",
    ),
    (
        "peko-protocol",
        "peko-subject",
        "protocol is serde+serde_json only; subject is a separate concern.",
    ),
    (
        "peko-protocol",
        "peko-provider-api",
        "protocol is serde+serde_json only; provider contracts are downstream.",
    ),
    (
        "peko-protocol",
        "peko-extension-api",
        "protocol is serde+serde_json only; extension hooks are downstream.",
    ),
    (
        "peko-protocol",
        "peko-extension-host",
        "protocol is serde+serde_json only; extension host is downstream.",
    ),
    (
        "peko-protocol",
        "peko-quota",
        "protocol is serde+serde_json only; quota is a runtime concern.",
    ),
    (
        "peko-protocol",
        "peko-events",
        "protocol is serde+serde_json only; agentic events are downstream.",
    ),
    (
        "peko-protocol",
        "peko-engine",
        "protocol is serde+serde_json only; engine is downstream.",
    ),
    (
        "peko-protocol",
        "peko-peko-daemon",
        "protocol is serde+serde_json only; the daemon is downstream.",
    ),
    # Phase 0.Z-B: protocol is serde+serde_json only.
    (
        "peko-protocol",
        "peko-peko-cli",
        "protocol is serde+serde_json only; the CLI is downstream.",
    ),
    # peko-subject is a pure value/type layer (Phase 3)
    (
        "peko-subject",
        "peko-message",
        "subject is a pure value layer; it must not depend on any other peko-* crate.",
    ),
    (
        "peko-subject",
        "peko-tools-core",
        "subject is a pure value layer.",
    ),
    (
        "peko-subject",
        "peko-extension-api",
        "subject is a pure value layer.",
    ),
    (
        "peko-subject",
        "peko-extension-host",
        "subject is a pure value layer.",
    ),
    (
        "peko-subject",
        "peko-provider-api",
        "subject is a pure value layer.",
    ),
    (
        "peko-subject",
        "peko-events",
        "subject is a pure value layer.",
    ),
    (
        "peko-subject",
        "peko-quota",
        "subject is a pure value layer.",
    ),
    (
        "peko-subject",
        "peko-protocol",
        "subject is a pure value layer.",
    ),
    (
        "peko-subject",
        "peko-engine",
        "subject is a pure value layer.",
    ),
    # peko-message is a pure message contract (Phase 2)
    (
        "peko-message",
        "peko-subject",
        "message is a pure contract.",
    ),
    (
        "peko-message",
        "peko-tools-core",
        "message is a pure contract.",
    ),
    (
        "peko-message",
        "peko-extension-api",
        "message is a pure contract.",
    ),
    (
        "peko-message",
        "peko-provider-api",
        "message is a pure contract.",
    ),
    (
        "peko-message",
        "peko-events",
        "message is a pure contract.",
    ),
    (
        "peko-message",
        "peko-quota",
        "message is a pure contract.",
    ),
    (
        "peko-message",
        "peko-protocol",
        "message is a pure contract.",
    ),
    (
        "peko-message",
        "peko-engine",
        "message is a pure contract.",
    ),
    # peko-tools-core is the tool API foundation (Phase 5)
    (
        "peko-tools-core",
        "peko-message",
        "tools-core is a pure API crate.",
    ),
    (
        "peko-tools-core",
        "peko-subject",
        "tools-core is a pure API crate.",
    ),
    (
        "peko-tools-core",
        "peko-extension-api",
        "tools-core is a pure API crate.",
    ),
    (
        "peko-tools-core",
        "peko-extension-host",
        "tools-core is a pure API crate.",
    ),
    (
        "peko-tools-core",
        "peko-provider-api",
        "tools-core is a pure API crate.",
    ),
    (
        "peko-tools-core",
        "peko-events",
        "tools-core is a pure API crate.",
    ),
    (
        "peko-tools-core",
        "peko-quota",
        "tools-core is a pure API crate.",
    ),
    (
        "peko-tools-core",
        "peko-protocol",
        "tools-core is a pure API crate.",
    ),
    (
        "peko-tools-core",
        "peko-engine",
        "tools-core is a pure API crate.",
    ),
    # peko-events is a neutral event contract (Phase 1)
    (
        "peko-events",
        "peko-message",
        "events is a neutral agentic event contract.",
    ),
    (
        "peko-events",
        "peko-subject",
        "events is a neutral agentic event contract.",
    ),
    (
        "peko-events",
        "peko-tools-core",
        "events is a neutral agentic event contract.",
    ),
    (
        "peko-events",
        "peko-extension-api",
        "events is a neutral agentic event contract.",
    ),
    (
        "peko-events",
        "peko-extension-host",
        "events is a neutral agentic event contract.",
    ),
    (
        "peko-events",
        "peko-provider-api",
        "events is a neutral agentic event contract.",
    ),
    (
        "peko-events",
        "peko-quota",
        "events is a neutral agentic event contract.",
    ),
    (
        "peko-events",
        "peko-protocol",
        "events is a neutral agentic event contract.",
    ),
    (
        "peko-events",
        "peko-engine",
        "events is a neutral agentic event contract.",
    ),
    # peko-quota depends only on peko-message (Phase 4)
    (
        "peko-quota",
        "peko-subject",
        "quota depends only on peko-message.",
    ),
    (
        "peko-quota",
        "peko-tools-core",
        "quota depends only on peko-message.",
    ),
    (
        "peko-quota",
        "peko-extension-api",
        "quota depends only on peko-message.",
    ),
    (
        "peko-quota",
        "peko-extension-host",
        "quota depends only on peko-message.",
    ),
    (
        "peko-quota",
        "peko-provider-api",
        "quota depends only on peko-message.",
    ),
    (
        "peko-quota",
        "peko-events",
        "quota depends only on peko-message.",
    ),
    (
        "peko-quota",
        "peko-protocol",
        "quota depends only on peko-message.",
    ),
    (
        "peko-quota",
        "peko-engine",
        "quota depends only on peko-message.",
    ),
    # peko-extension-api is the stable framework contract (Phase 7)
    (
        "peko-extension-api",
        "peko-extension-host",
        "extension-api must not depend on its implementation.",
    ),
    (
        "peko-extension-api",
        "peko-engine",
        "extension-api is a contract crate; engine is downstream.",
    ),
    (
        "peko-extension-api",
        "peko-protocol",
        "extension-api is a contract crate; protocol is a separate wire contract.",
    ),
    (
        "peko-extension-api",
        "peko-events",
        "extension-api is a contract crate; events is downstream.",
    ),
    (
        "peko-extension-api",
        "peko-quota",
        "extension-api is a contract crate; quota is downstream.",
    ),
    # peko-fs-persistence is a leaf utility crate (Phase 5)
    (
        "peko-fs-persistence",
        "peko-subject",
        "fs-persistence is leaf-utility; no peko-* deps allowed.",
    ),
    (
        "peko-fs-persistence",
        "peko-message",
        "fs-persistence is leaf-utility; no peko-* deps allowed.",
    ),
    (
        "peko-fs-persistence",
        "peko-tools-core",
        "fs-persistence is leaf-utility; no peko-* deps allowed.",
    ),
    (
        "peko-fs-persistence",
        "peko-extension-api",
        "fs-persistence is leaf-utility; no peko-* deps allowed.",
    ),
    (
        "peko-fs-persistence",
        "peko-extension-host",
        "fs-persistence is leaf-utility; no peko-* deps allowed.",
    ),
    (
        "peko-fs-persistence",
        "peko-protocol",
        "fs-persistence is leaf-utility; no peko-* deps allowed.",
    ),
    (
        "peko-fs-persistence",
        "peko-engine",
        "fs-persistence is leaf-utility; no peko-* deps allowed.",
    ),
    (
        "peko-fs-persistence",
        "peko-events",
        "fs-persistence is leaf-utility; no peko-* deps allowed.",
    ),
    (
        "peko-fs-persistence",
        "peko-quota",
        "fs-persistence is leaf-utility; no peko-* deps allowed.",
    ),
    (
        "peko-fs-persistence",
        "peko-identity",
        "fs-persistence is leaf-utility; no peko-* deps allowed.",
    ),
    (
        "peko-fs-persistence",
        "peko-auth",
        "fs-persistence is leaf-utility; no peko-* deps allowed.",
    ),
    (
        "peko-fs-persistence",
        "peko-provider-api",
        "fs-persistence is leaf-utility; no peko-* deps allowed.",
    ),
    (
        "peko-fs-persistence",
        "peko-peko-daemon",
        "fs-persistence is leaf-utility; no peko-* deps allowed.",
    ),
    # Phase 0.Z-B
    (
        "peko-fs-persistence",
        "peko-peko-cli",
        "fs-persistence is leaf-utility; no peko-* deps allowed.",
    ),
    (
        "peko-fs-persistence",
        "peko",
        "fs-persistence is leaf-utility; no peko-* deps allowed.",
    ),
    # peko-plan is a leaf domain crate (Phase X).
    # Allowed deps: peko-subject (PrincipalId), peko-fs-persistence (FileLock).
    (
        "peko-plan",
        "peko-message",
        "plan is a leaf domain crate; only peko-subject + peko-fs-persistence allowed.",
    ),
    (
        "peko-plan",
        "peko-tools-core",
        "plan is a leaf domain crate; only peko-subject + peko-fs-persistence allowed.",
    ),
    (
        "peko-plan",
        "peko-extension-api",
        "plan is a leaf domain crate; only peko-subject + peko-fs-persistence allowed.",
    ),
    (
        "peko-plan",
        "peko-extension-host",
        "plan is a leaf domain crate; only peko-subject + peko-fs-persistence allowed.",
    ),
    (
        "peko-plan",
        "peko-protocol",
        "plan is a leaf domain crate; only peko-subject + peko-fs-persistence allowed.",
    ),
    (
        "peko-plan",
        "peko-engine",
        "plan is a leaf domain crate; only peko-subject + peko-fs-persistence allowed.",
    ),
    (
        "peko-plan",
        "peko-events",
        "plan is a leaf domain crate; only peko-subject + peko-fs-persistence allowed.",
    ),
    (
        "peko-plan",
        "peko-quota",
        "plan is a leaf domain crate; only peko-subject + peko-fs-persistence allowed.",
    ),
    (
        "peko-plan",
        "peko-identity",
        "plan is a leaf domain crate; only peko-subject + peko-fs-persistence allowed.",
    ),
    (
        "peko-plan",
        "peko-auth",
        "plan is a leaf domain crate; only peko-subject + peko-fs-persistence allowed.",
    ),
    (
        "peko-plan",
        "peko-provider-api",
        "plan is a leaf domain crate; only peko-subject + peko-fs-persistence allowed.",
    ),
    (
        "peko-plan",
        "peko-providers",
        "plan is a leaf domain crate; only peko-subject + peko-fs-persistence allowed.",
    ),
    (
        "peko-plan",
        "peko-observability",
        "plan is a leaf domain crate; only peko-subject + peko-fs-persistence allowed.",
    ),
    (
        "peko-plan",
        "peko-peko-daemon",
        "plan is a leaf domain crate; only peko-subject + peko-fs-persistence allowed.",
    ),
    (
        "peko-plan",
        "peko-peko-cli",
        "plan is a leaf domain crate; only peko-subject + peko-fs-persistence allowed.",
    ),
    (
        "peko-plan",
        "peko-cron",
        "plan is a leaf domain crate; only peko-subject + peko-fs-persistence allowed.",
    ),
    (
        "peko-plan",
        "peko-session",
        "plan is a leaf domain crate; only peko-subject + peko-fs-persistence allowed.",
    ),
    (
        "peko-plan",
        "peko",
        "plan is a leaf domain crate; only peko-subject + peko-fs-persistence allowed.",
    ),
]


# ---------------------------------------------------------------------------
# Minimal TOML walker
# ---------------------------------------------------------------------------
#
# Cargo.toml's ``[dependencies]`` section is a flat list of
# ``name = { path = "...", ... }`` entries. We don't need a full TOML
# parser to extract them — a single regex is enough. Multi-line
# inline tables (``{ path = "..." }``) are handled because we split
# on ``\n`` first and then match each line.


_DEP_LINE = re.compile(
    r"^\s*([A-Za-z0-9_\-]+)\s*=\s*(.+?)\s*(?:#.*)?$",
    re.MULTILINE,
)


def _extract_section(text: str, section: str) -> List[Tuple[str, str]]:
    """Return ``[(name, raw_value)]`` pairs for the named TOML section.

    Only flat ``name = value`` pairs are returned; nested sections
    (``[section.sub]``) are ignored. This is enough for Cargo.toml
    ``[dependencies]``-family sections.
    """
    lines = text.split("\n")
    in_section = False
    section_prefix = f"[{section}]"
    section_indent = ""
    pairs: List[Tuple[str, str]] = []
    for raw in lines:
        line = raw.rstrip()
        stripped = line.strip()
        if not stripped:
            continue
        if stripped.startswith("#"):
            continue
        # Section header? Match exactly `[name]` or `[name.something]`
        # but only enter when the prefix matches exactly.
        if stripped.startswith("[") and stripped.endswith("]"):
            name = stripped[1:-1].strip()
            if name == section:
                in_section = True
                section_indent = ""
                continue
            # Are we descending into a sub-section we don't care about?
            if in_section and name.startswith(section + "."):
                continue
            # A sibling section ends ours.
            in_section = False
            continue
        if not in_section:
            continue
        # Skip inline-table / multi-line continuations that don't start
        # at column 0 (we only want `name = ...` lines, not `key = ...`
        # inside an inline table).
        if line.startswith(" ") or line.startswith("\t"):
            continue
        # Strip a possible array-of-tables header `[[name]]`.
        match = _DEP_LINE.match(line)
        if not match:
            continue
        name, value = match.group(1), match.group(2)
        # Skip table-style dependencies (those would be `name = { ... }`
        # AND start at column 0; that's fine — we want both `path = "..."`
        # and `version = "..."` forms).
        pairs.append((name, value))
    return pairs


def _is_peko_dep(name: str) -> bool:
    return name.startswith("peko-") or name == "peko"


def _parse_peko_deps(cargo_toml_path: Path) -> Dict[str, List[str]]:
    """Return ``{section: [peko_crate_names]}`` for the named Cargo.toml.

    Sections checked: ``dependencies``, ``dev-dependencies``,
    ``build-dependencies``. Each returned list contains only
    workspace-internal ``peko-*`` deps.
    """
    text = cargo_toml_path.read_text()
    result: Dict[str, List[str]] = {}
    for section in ("dependencies", "dev-dependencies", "build-dependencies"):
        pairs = _extract_section(text, section)
        peko = sorted({name for name, _ in pairs if _is_peko_dep(name)})
        if peko:
            result[section] = peko
    return result


# ---------------------------------------------------------------------------
# Cargo.toml crate-name resolution
# ---------------------------------------------------------------------------


def _crate_name_for(cargo_toml_path: Path, fallback: str) -> str:
    """Return the ``[package].name`` declared in the given Cargo.toml.

    Used to translate each ``peko-rs/<dir>/Cargo.toml`` → its
    ``[package].name`` so the dep edges line up with the
    ``FORBIDDEN_EDGES`` table. Most sats follow the
    ``peko-<dir>`` convention; a few (CLI, daemon-bin) override via
    ``package = "peko"`` in the root lib's Cargo.toml.
    """
    text = cargo_toml_path.read_text()
    in_package = False
    for raw in text.split("\n"):
        line = raw.strip()
        if line == "[package]":
            in_package = True
            continue
        if in_package and line.startswith("[") and line.endswith("]"):
            break
        if in_package and line.startswith("name"):
            match = re.match(r'name\s*=\s*"([^"]+)"', line)
            if match:
                return match.group(1)
    return fallback


def _has_package_section(cargo_toml: Path) -> bool:
    """Return True iff ``cargo_toml`` contains a top-level ``[package]`` section.

    Phase 0.Z-D made the root ``Cargo.toml`` workspace-only (no
    ``[package]``). The walker therefore has to detect whether the root
    is still a real member (legacy layout) or a thin workspace manifest
    (post-0.Z-D layout) and skip it in the latter case.
    """
    try:
        text = cargo_toml.read_text(encoding="utf-8")
    except OSError:
        return False
    for raw in text.splitlines():
        stripped = raw.strip()
        if stripped.startswith("#"):
            continue
        if stripped == "[package]":
            return True
    return False


# ---------------------------------------------------------------------------
# Walker
# ---------------------------------------------------------------------------


def discover_workspace_members(repo_root: Path) -> List[Tuple[str, Path]]:
    """Return ``[(crate_name, cargo_toml_path), ...]`` for every workspace member.

    After Phase 0.Z-D the root ``Cargo.toml`` is workspace-only (no
    ``[package]``); the canonical home for every crate — including
    ``peko_core`` — is ``peko-rs/<name>/Cargo.toml``. The walker appends
    the root only when it carries a ``[package]`` block, so it stays
    compatible with both the pre-0.Z-D and post-0.Z-D layouts.
    """
    members: List[Tuple[str, Path]] = []
    root_cargo = repo_root / "Cargo.toml"
    if root_cargo.exists() and _has_package_section(root_cargo):
        members.append((_crate_name_for(root_cargo, "peko"), root_cargo))
    crates_dir = repo_root / "peko-rs"
    if not crates_dir.is_dir():
        return members
    for entry in sorted(crates_dir.iterdir()):
        if not entry.is_dir():
            continue
        cargo = entry / "Cargo.toml"
        if not cargo.exists():
            continue
        members.append((_crate_name_for(cargo, entry.name), cargo))
    return members


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[1])
    parser.add_argument(
        "--repo-root",
        type=Path,
        default=Path(__file__).resolve().parent.parent,
        help="Path to the peko-runtime repo root (default: parent of scripts/).",
    )
    parser.add_argument(
        "--print-graph",
        action="store_true",
        help="Print the actual dependency graph (dot-style) before checking.",
    )
    args = parser.parse_args()

    repo_root: Path = args.repo_root.resolve()
    members = discover_workspace_members(repo_root)
    if not members:
        print(f"ERROR: no workspace members found under {repo_root}", file=sys.stderr)
        return 1

    # Build the edge map: crate_name → set of peko-* crate names it depends on.
    # The edge map is populated in iteration order, so a dep that appears
    # before its target crate in `members` is still added (we don't gate on
    # target presence; the forbidden-edge table is the authoritative gate).
    edges: Dict[str, Set[str]] = {}
    edge_source: Dict[Tuple[str, str], str] = {}
    for crate_name, cargo_path in members:
        sections = _parse_peko_deps(cargo_path)
        edges.setdefault(crate_name, set())
        for section, names in sections.items():
            for dep in names:
                if dep == crate_name:
                    continue
                edges[crate_name].add(dep)
                edge_source.setdefault((crate_name, dep), section)

    if args.print_graph:
        print("Workspace dependency graph (peko-* edges only):")
        print("-" * 60)
        for crate_name in sorted(edges):
            deps = sorted(edges[crate_name])
            if not deps:
                print(f"  {crate_name}  (no peko-* deps)")
            else:
                print(f"  {crate_name} → {', '.join(deps)}")
        print()

    # Assert forbidden edges.
    forbidden_set = {(a, b): reason for a, b, reason in FORBIDDEN_EDGES}
    violations: List[Tuple[str, str, str, str]] = []  # (a, b, section, reason)
    for (a, b), reason in forbidden_set.items():
        if b in edges.get(a, set()):
            section = edge_source.get((a, b), "dependencies")
            violations.append((a, b, section, reason))

    if violations:
        print("=" * 60)
        print(f"FAIL: {len(violations)} forbidden workspace edge(s) detected")
        print("=" * 60)
        for a, b, section, reason in violations:
            print(f"\n  {a} --[{section}]--> {b}")
            print(f"    reason: {reason}")
        print(
            "\nFix: remove the offending dependency, or update "
            "FORBIDDEN_EDGES in scripts/check_workspace_deps.py if the "
            "rule no longer applies (and document the change in the PR)."
        )
        return 1

    total_edges = sum(len(d) for d in edges.values())
    print(
        f"OK: {len(members)} workspace members, {total_edges} peko-* edge(s), "
        f"{len(FORBIDDEN_EDGES)} forbidden-edge rule(s) all clean."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())