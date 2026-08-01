#!/usr/bin/env bash
# scripts/e2e/clean-tmp.sh
#
# Sweep stale e2e-isolation tempdirs left behind by `peko_iso_init`.
# Targets the two `iso_root` candidates the lib uses (PEKO_ISO_ROOT env
# override, `/tmp/peko`, or `<repo>/target/e2e`) and removes any
# tempdir matching the lib's `<flow>-<pid>-<rand>` naming pattern that
# is NOT currently in use by a live `peko` daemon.
#
# When tempdirs accumulate:
#   - KEEP_TEMPDIR=1 runs (intentional, but leaks)
#   - SIGKILL on the parent shell (the lib's EXIT/INT/TERM trap can't fire)
#   - SIGKILL on a `peko daemon` whose pidfile escapes the lib's tracker
#
# Usage:
#   scripts/e2e/clean-tmp.sh              # dry-run, list candidates + sizes
#   scripts/e2e/clean-tmp.sh --apply      # actually remove
#   scripts/e2e/clean-tmp.sh --min-age 6  # only clean dirs ≥6h old (default 0)
#   scripts/e2e/clean-tmp.sh --root <p>   # override iso_root (repeatable)
#   scripts/e2e/clean-tmp.sh --quiet      # summary only (no per-dir output)
#
# Exit codes:
#   0  no candidates / dry-run finished / clean succeeded
#   1  some candidates skipped because a live peko daemon still holds them
#   2  --apply failed (permission, I/O, etc.)

# Tolerate set -u callers.
set +u

# --- option parsing --------------------------------------------------------

APPLY=0
QUIET=0
MIN_AGE_MIN=0
declare -a CUSTOM_ROOTS=()

while [[ $# -gt 0 ]]; do
  case "$1" in
    --apply)     APPLY=1; shift ;;
    --quiet|-q)  QUIET=1; shift ;;
    --min-age)   MIN_AGE_MIN="${2:-0}"; shift 2 ;;
    --min-age=*) MIN_AGE_MIN="${1#*=}"; shift ;;
    --root)      CUSTOM_ROOTS+=("${2:-}"); shift 2 ;;
    --root=*)    CUSTOM_ROOTS+=("${1#*=}"); shift ;;
    -h|--help)
      # Print the leading doc-comment block. Skip the `#!` shebang,
      # stop at the first non-`#` line, then strip the leading `#`.
      awk 'NR>1 && /^[^#]/{exit} NR>1' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *)
      echo "❌ unknown flag: $1" >&2
      exit 64   # EX_USAGE
      ;;
  esac
done

# --- derive iso_root candidates --------------------------------------------

# Match the lib's resolution order:
#   PEKO_ISO_ROOT (env override) → /tmp/peko → <repo>/target/e2e
declare -a ROOTS=()

if [[ ${#CUSTOM_ROOTS[@]} -gt 0 ]]; then
  ROOTS=("${CUSTOM_ROOTS[@]}")
else
  if [[ -n "${PEKO_ISO_ROOT:-}" ]]; then
    ROOTS+=("$PEKO_ISO_ROOT")
  fi
  if [[ -d /tmp/peko ]] || [[ -w /tmp ]]; then
    ROOTS+=("/tmp/peko")
  fi
  # Repo fallback: <repo>/target/e2e. Self-locate from this script's path
  # so the script doesn't have to be run from the repo root.
  script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  repo_root="$(cd "$script_dir/../.." && pwd)"
  if [[ -d "$repo_root/target/e2e" ]]; then
    ROOTS+=("$repo_root/target/e2e")
  fi
fi

# De-dupe while preserving order.
declare -a SEEN=()
declare -a UNIQUE_ROOTS=()
for r in "${ROOTS[@]}"; do
  [[ -z "$r" ]] && continue
  if [[ ! " ${SEEN[*]} " =~ \ $r\  ]]; then
    SEEN+=("$r")
    UNIQUE_ROOTS+=("$r")
  fi
done

if [[ ${#UNIQUE_ROOTS[@]} -eq 0 ]]; then
  echo "ℹ️  no iso_root candidates exist — nothing to sweep"
  exit 0
fi

# --- helpers ---------------------------------------------------------------

PEKO_ISO_PATTERN='-[0-9]+-[a-z0-9]{6}$'

is_peko_alive() {
  local pid="$1"
  [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null
}

dir_size_bytes() {
  du -sk "$1" 2>/dev/null | awk '{print $1 * 1024}'
}

# Return age in minutes via stat (BSD stat -f on macOS, GNU stat -c on Linux).
age_min() {
  local path="$1" mtime now age_s
  mtime="$(stat -f '%m' "$path" 2>/dev/null || stat -c '%Y' "$path" 2>/dev/null)"
  [[ -z "$mtime" ]] && { echo "-1"; return; }
  now=$(date +%s)
  age_s=$(( now - mtime ))
  echo $(( age_s / 60 ))
}

human_size() {
  numfmt --to=iec "$1" 2>/dev/null || echo "${1}B"
}

# --- sweep -----------------------------------------------------------------

CANDIDATES=0
SKIPPED_LIVE=0
SKIPPED_AGE=0
REMOVED=0
BYTES_RECLAIMED=0
declare -a FAILURES=()

for root in "${UNIQUE_ROOTS[@]}"; do
  if [[ ! -d "$root" ]]; then
    (( QUIET )) || echo "⏭  $root (does not exist)"
    continue
  fi

  (( QUIET )) || echo "─── sweep $root ───"

  # 1-level scan: only direct children of iso_root. Nested tempdirs (a
  # flow running inside another) aren't a thing — the lib always uses
  # iso_root as the parent.
  shopt -s nullglob dotglob
  for entry in "$root"/*; do
    [[ -d "$entry" ]] || continue
    base="$(basename "$entry")"

    # Pattern gate — anything not matching the lib's `<flow>-<pid>-<rand>`
    # shape is somebody else's dir (or a stale partial from a buggy
    # version). Skip silently.
    if ! [[ "$base" =~ $PEKO_ISO_PATTERN ]]; then
      continue
    fi

    CANDIDATES=$(( CANDIDATES + 1 ))

    # Live-pid gate: if a daemon.pid inside still resolves to a live
    # process, the user is mid-run (or about to inspect) — skip.
    pidfile="$entry/home/.peko/run/daemon.pid"
    if [[ -f "$pidfile" ]]; then
      pid="$(cat "$pidfile" 2>/dev/null || true)"
      if is_peko_alive "$pid"; then
        (( QUIET )) || echo "  ⏭  $entry  (live daemon pid=$pid)"
        SKIPPED_LIVE=$(( SKIPPED_LIVE + 1 ))
        continue
      fi
    fi

    # Age gate (default off; activated by --min-age).
    if (( MIN_AGE_MIN > 0 )); then
      this_age="$(age_min "$entry")"
      if (( this_age >= 0 )) && (( this_age < MIN_AGE_MIN )); then
        (( QUIET )) || echo "  ⏭  $entry  (age=${this_age}m < ${MIN_AGE_MIN}m)"
        SKIPPED_AGE=$(( SKIPPED_AGE + 1 ))
        continue
      fi
    fi

    bytes="$(dir_size_bytes "$entry")"
    size_human="$(human_size "$bytes")"

    if (( APPLY )); then
      if rm -rf "$entry" 2>/dev/null; then
        (( QUIET )) || echo "  ✓  $entry  (${size_human})"
        REMOVED=$(( REMOVED + 1 ))
        BYTES_RECLAIMED=$(( BYTES_RECLAIMED + bytes ))
      else
        echo "  ✗  $entry  (rm failed)" >&2
        FAILURES+=("$entry")
      fi
    else
      (( QUIET )) || echo "  ·  $entry  (${size_human}, would remove)"
    fi
  done
  shopt -u nullglob dotglob
done

# --- summary ---------------------------------------------------------------

reclaim_human="$(human_size "$BYTES_RECLAIMED")"

echo
echo "─── summary ───"
echo "  candidates           : $CANDIDATES"
echo "  skipped (live daemon) : $SKIPPED_LIVE"
if (( MIN_AGE_MIN > 0 )); then
  echo "  skipped (age<${MIN_AGE_MIN}m): $SKIPPED_AGE"
fi
if (( APPLY )); then
  echo "  removed              : $REMOVED  (${reclaim_human})"
  echo "  failed               : ${#FAILURES[@]}"
else
  echo "  removed              : 0  (dry-run; pass --apply to actually delete)"
fi

if (( APPLY )) && (( ${#FAILURES[@]} > 0 )); then
  printf '  failure: %s\n' "${FAILURES[@]}" >&2
  exit 2
fi

if (( SKIPPED_LIVE > 0 )); then
  exit 1
fi

exit 0