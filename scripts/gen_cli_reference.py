#!/usr/bin/env python3
"""Generate docs/user-guide/CLI_REFERENCE.md from the CLI's own --help.

Usage: python3 scripts/gen_cli_reference.py [path/to/peko] > docs/user-guide/CLI_REFERENCE.md

Walks every command `peko --help` lists (and their subcommands, one level
down), and prints each command's long help. The global flags every command
repeats are printed once, at the top. Hidden commands (`runtime`, `config`,
`tunnel`, `vault`, ...) are not listed by `--help` and are left out.
"""

import re
import subprocess
import sys

PEKO = sys.argv[1] if len(sys.argv) > 1 else "peko"

# Option lines (as clap renders them) that belong to the global flag set.
GLOBAL_FLAGS = (
    "--config-dir",
    "--data-dir",
    "--cache-dir",
    "--json",
    "-q, --quiet",
    "-v, --verbose",
    "--debug",
    "-h, --help",
    "-V, --version",
)


def run_help(args):
    out = subprocess.run(
        [PEKO, *args, "--help"], capture_output=True, text=True, check=True
    ).stdout
    return out.rstrip("\n")


def subcommands(help_text):
    """Names listed under a `Commands:` heading, minus `help`."""
    names, in_block = [], False
    for line in help_text.splitlines():
        if line.strip() == "Commands:":
            in_block = True
            continue
        if in_block:
            m = re.match(r"^  ([a-z][a-z0-9-]*)\s", line + " ")
            if m:
                if m.group(1) != "help":
                    names.append(m.group(1))
            elif line and not line.startswith("    "):
                break
    return names


def strip_globals(help_text):
    """Drop the global flags clap repeats in every command's Options."""
    out, skipping = [], False
    for line in help_text.splitlines():
        stripped = line.strip()
        is_option = line.startswith("  -") or line.startswith("      --")
        if is_option:
            skipping = any(stripped.startswith(flag) for flag in GLOBAL_FLAGS)
        elif skipping and (not stripped or line.startswith("          ")):
            continue  # description (blank lines included) of a skipped option
        else:
            skipping = False
        if skipping:
            continue
        out.append(line)
    text = "\n".join(out).rstrip()
    return re.sub(r"\nOptions:\s*$", "", text)


def main():
    top = run_help([])
    version = subprocess.run(
        [PEKO, "--version"], capture_output=True, text=True, check=True
    ).stdout.strip()
    print("# Peko CLI Reference")
    print()
    print(
        "Generated from the CLI's own `--help` output by "
        "`scripts/gen_cli_reference.py` "
        f"({version}). Regenerate it after changing any command; "
        "`peko <command> --help` shows the same text."
    )
    print()
    print("## `peko`")
    print()
    print("```text")
    print(top)
    print("```")
    for name in subcommands(top):
        text = run_help([name])
        print()
        print(f"## `peko {name}`")
        print()
        print("```text")
        print(strip_globals(text))
        print("```")
        for sub in subcommands(text):
            print()
            print(f"### `peko {name} {sub}`")
            print()
            print("```text")
            print(strip_globals(run_help([name, sub])))
            print("```")


if __name__ == "__main__":
    main()
