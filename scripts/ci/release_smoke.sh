#!/usr/bin/env bash
# Release smoke test: install a published release the way a user does, then
# run the quick start against a local fake model.
#
#   scripts/ci/release_smoke.sh install <version> <install-dir>
#   scripts/ci/release_smoke.sh check <install-dir>
#
# `install` runs this checkout's install.sh against the GitHub release
# (version with or without a leading `v`). `check` needs no network or key:
# it serves a fake OpenAI-compatible endpoint (scripts/ci/fake_llm.py) and
# drives model add (--no-key) → model test → daemon start → create → send →
# log → export → daemon stop. It also checks two regressions:
#   - keyless models run turns and send no Authorization header (#498);
#   - `peko export` writes into the caller's directory, not the daemon's (#497).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

step() { printf '\n=== %s\n' "$*"; }
die() { printf '\nFAIL: %s\n' "$*" >&2; exit 1; }

cmd_install() {
    local version="${1#v}" dir="$2"
    # Don't create `dir`: install.sh must create a missing install directory.
    step "install.sh --version $version --install-dir $dir"
    bash "$ROOT/install.sh" --version "$version" --install-dir "$dir"
    [ -x "$dir/peko" ] || die "peko not installed in $dir"
    [ -x "$dir/peko-daemon" ] || die "peko-daemon not installed in $dir"
}

cmd_check() {
    local dir="$1"
    export PATH="$dir:$PATH"
    export PEKO_UNLOCK_METHOD=passphrase PEKO_MASTER_PASSPHRASE=release-smoke
    export NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost

    # Script-level, not `local`: the EXIT trap runs after this function returns.
    work="$(mktemp -d)"
    port="${SMOKE_PORT:-18997}"
    log="$work/fake-llm.jsonl"
    fake_pid=""
    mkdir -p "$work/daemon-cwd" "$work/export-cwd"

    # Own output file, so the server never holds the caller's stdout open.
    python3 "$ROOT/scripts/ci/fake_llm.py" "$port" "$log" >"$work/fake-llm.out" 2>&1 &
    fake_pid=$!
    cleanup() {
        local status=$?
        if [ "$status" -ne 0 ]; then
            echo "--- daemon log (tail)"
            tail -40 "$HOME/.peko/logs/daemon.log" 2>/dev/null || true
        fi
        (cd "$work/daemon-cwd" && peko daemon stop >/dev/null 2>&1) || true
        # `python3` can be a launcher shim (macOS), so stop the server by its
        # command line as well as by pid.
        kill "$fake_pid" 2>/dev/null || true
        pkill -f "fake_llm.py $port" 2>/dev/null || true
        exit "$status"
    }
    trap cleanup EXIT
    for _ in $(seq 1 50); do
        curl -fsS --noproxy '*' "http://127.0.0.1:$port/v1/models" >/dev/null 2>&1 && break
        sleep 0.2
    done

    step "peko --version"
    peko --version | grep -q '^peko ' || die "unexpected --version output"

    step "peko model add --no-key"
    peko model add --id smoke --api-format openai_completions \
        --base-url "http://127.0.0.1:$port/v1" --model fake --no-key

    step "peko model test"
    peko model test smoke

    step "peko daemon start"
    (cd "$work/daemon-cwd" && peko daemon start)
    peko daemon status | grep -q 'Running' || die "daemon not running"

    step "peko create"
    peko create smoke-peko --model smoke --wait-timeout 240

    step "peko send"
    local reply
    reply="$(peko send smoke-peko "hello")"
    echo "$reply"
    grep -q 'ok' <<<"$reply" || die "no reply from the peko"

    step "peko log"
    peko log smoke-peko --limit 5 | tee "$work/log.txt"
    grep -q 'hello' "$work/log.txt" || die "the message is not in the log"

    step "peko export (from a directory the daemon did not start in)"
    (cd "$work/export-cwd" && peko export smoke-peko)
    [ -f "$work/export-cwd/smoke-peko.peko" ] || die "export did not land in the caller's directory"

    step "keyless calls carry no Authorization header"
    python3 - "$log" <<'PY'
import json, sys
posts = [json.loads(l) for l in open(sys.argv[1]) if json.loads(l)["method"] == "POST"]
assert posts, "the fake model received no chat calls"
assert not any(p["auth"] for p in posts), "a keyless call sent an Authorization header"
print(f"{len(posts)} chat calls, none with an Authorization header")
PY

    step "peko daemon stop"
    (cd "$work/daemon-cwd" && peko daemon stop)
    echo
    echo "release smoke test passed"
}

case "${1:-}" in
    install) shift; cmd_install "$@" ;;
    check) shift; cmd_check "$@" ;;
    *) echo "usage: $0 install <version> <dir> | check <dir>" >&2; exit 2 ;;
esac
