#!/usr/bin/env python3
"""Set the Peko version everywhere it is written (ADR-102).

    python3 scripts/bump_version.py patch      # 0.1.0 -> 0.1.1
    python3 scripts/bump_version.py minor      # 0.1.4 -> 0.2.0
    python3 scripts/bump_version.py 0.3.0      # an exact version
    python3 scripts/bump_version.py --print    # print the current version
    python3 scripts/bump_version.py --check    # every manifest agrees?

A bump edits every workspace manifest under `peko-rs/`, the workspace's own
entries in `Cargo.lock`, and `CHANGELOG.md` (what is under `[Unreleased]`
becomes the new version's dated section). Merge the result as a pull request:
`release.yml` tags and releases the new version when it reaches `master`.

Patch: nothing a client depends on breaks. Minor: it may. See ADR-102 D2.
"""

from __future__ import annotations

import datetime
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
SOURCE_OF_TRUTH = ROOT / "peko-rs" / "core" / "Cargo.toml"
VERSION_RE = re.compile(r"^\d+\.\d+\.\d+$")
PACKAGE_VERSION_RE = re.compile(r'^version = "([^"]+)"$', re.MULTILINE)
PACKAGE_NAME_RE = re.compile(r'^name = "([^"]+)"$', re.MULTILINE)


def manifests() -> list[pathlib.Path]:
    return sorted((ROOT / "peko-rs").glob("*/Cargo.toml"))


def package_section(text: str) -> str:
    """The `[package]` table's text, so a dependency's version is never read."""
    start = text.index("[package]")
    end = text.find("\n[", start + 1)
    return text[start : end if end != -1 else len(text)]


def read_version(manifest: pathlib.Path) -> str:
    match = PACKAGE_VERSION_RE.search(package_section(manifest.read_text()))
    if not match:
        sys.exit(f"error: no package version in {manifest.relative_to(ROOT)}")
    return match.group(1)


def read_name(manifest: pathlib.Path) -> str:
    match = PACKAGE_NAME_RE.search(package_section(manifest.read_text()))
    if not match:
        sys.exit(f"error: no package name in {manifest.relative_to(ROOT)}")
    return match.group(1)


def next_version(current: str, how: str) -> str:
    if VERSION_RE.match(how):
        return how
    if not VERSION_RE.match(current):
        sys.exit(f"error: current version {current!r} is not X.Y.Z; pass an exact version")
    major, minor, patch = (int(part) for part in current.split("."))
    if how == "patch":
        return f"{major}.{minor}.{patch + 1}"
    if how == "minor":
        return f"{major}.{minor + 1}.0"
    if how == "major":
        return f"{major + 1}.0.0"
    sys.exit(f"error: expected patch, minor, major or X.Y.Z, got {how!r}")


def as_tuple(version: str) -> tuple[int, int, int]:
    major, minor, patch = (int(part) for part in version.split("."))
    return (major, minor, patch)


def set_manifest_version(manifest: pathlib.Path, new: str) -> None:
    text = manifest.read_text()
    section = package_section(text)
    updated = PACKAGE_VERSION_RE.sub(f'version = "{new}"', section, count=1)
    manifest.write_text(text.replace(section, updated, 1))


def set_lock_versions(names: set[str], old: str, new: str) -> int:
    """Rewrite the workspace packages' own entries in Cargo.lock."""
    lock = ROOT / "Cargo.lock"
    lines = lock.read_text().split("\n")
    changed = 0
    for i, line in enumerate(lines[:-1]):
        match = re.match(r'^name = "([^"]+)"$', line)
        if match and match.group(1) in names and lines[i + 1] == f'version = "{old}"':
            # A registry package has a `source` line; a workspace member does not.
            if i + 2 < len(lines) and lines[i + 2].startswith("source = "):
                continue
            lines[i + 1] = f'version = "{new}"'
            changed += 1
    lock.write_text("\n".join(lines))
    return changed


def cut_changelog(new: str) -> str:
    """Move what is under `[Unreleased]` into a dated section for `new`."""
    path = ROOT / "CHANGELOG.md"
    text = path.read_text()
    marker = "## [Unreleased]\n"
    if marker not in text:
        return "CHANGELOG.md has no [Unreleased] section; left alone"
    today = datetime.date.today().isoformat()
    heading = f"## [{new}] - {today}\n"
    if re.search(rf"^## \[{re.escape(new)}\]", text, re.MULTILINE):
        return f"CHANGELOG.md already has a [{new}] section; fold [Unreleased] into it by hand"
    head, rest = text.split(marker, 1)
    next_section = re.search(r"^## \[", rest, re.MULTILINE)
    body = rest[: next_section.start()] if next_section else rest
    tail = rest[next_section.start() :] if next_section else ""
    if not body.strip():
        body = "\nNo user-facing changes.\n\n"
    path.write_text(head + marker + "\n" + heading + body + tail)
    return f"CHANGELOG.md: opened [{new}] - {today}"


def check() -> int:
    versions = {m: read_version(m) for m in manifests()}
    truth = versions[SOURCE_OF_TRUTH]
    wrong = {m: v for m, v in versions.items() if v != truth}
    for manifest, version in wrong.items():
        print(f"error: {manifest.relative_to(ROOT)} says {version}, core says {truth}")
    if not wrong:
        print(f"ok: {len(versions)} manifests at {truth}")
    return 1 if wrong else 0


def main(argv: list[str]) -> int:
    if len(argv) != 2 or argv[1] in ("-h", "--help"):
        print(__doc__)
        return 0 if len(argv) == 2 else 2
    if argv[1] == "--print":
        print(read_version(SOURCE_OF_TRUTH))
        return 0
    if argv[1] == "--check":
        return check()

    if check() != 0:
        return 1
    current = read_version(SOURCE_OF_TRUTH)
    new = next_version(current, argv[1])
    if VERSION_RE.match(current) and as_tuple(new) <= as_tuple(current):
        sys.exit(f"error: {new} is not after the current version {current}")

    names = set()
    for manifest in manifests():
        names.add(read_name(manifest))
        set_manifest_version(manifest, new)
    locked = set_lock_versions(names, current, new)
    if locked != len(names):
        sys.exit(
            f"error: Cargo.lock had {locked} of {len(names)} workspace packages at "
            f"{current}; run `cargo metadata` and check it by hand"
        )
    note = cut_changelog(new)

    print(f"{current} -> {new}")
    print(f"  {len(names)} manifests under peko-rs/, and their Cargo.lock entries")
    print(f"  {note}")
    print("Next: commit, open a pull request, merge. release.yml tags and")
    print(f"releases v{new} when the change reaches master (ADR-102 D3).")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
