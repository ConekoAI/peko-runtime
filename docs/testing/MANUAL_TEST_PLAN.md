# Peko Desktop × Peko Runtime — Manual Test Plan

> **Status:** v0.4 (provider/credential UI, per-message overrides)
> **Last updated:** 2026-07-16
> **Maintained by:** Peko engineering
> **Audience:** non-technical testers verifying core functionality before the next feature drop
>
> **What changed in v0.4**
> - **RP6** restructured Settings → Credentials into an accordion of provider
>   cards. Providers can be edited, removed, enabled/disabled, and set as the
>   runtime default. Each provider card expands to show its stored
>   credentials, an "add credential" form, and a rotation-binding panel.
> - **RP7** added a model dropdown to the **Create a Principal** modal. After
>   picking a provider, the user can optionally pin a model; otherwise the
>   principal inherits the provider's default model.
> - **RP8** added `--provider` and `--model` flags to `peko send`, letting a
>   single message override the resolved provider/model without changing the
>   principal's configuration.
>
> **What changed in v0.3 (engine adoption)**
> The desktop now co-exists with `peko daemon start` from the CLI. If a
> `peko` daemon is already on the IPC socket when the desktop launches, the
> sidecar supervisor **adopts** it (mirrors its state, does not spawn a
> child) and the chat works normally. The desktop's own sidecar is only
> spawned if no daemon is running.
>
> Visible change for the user: the engine is **invisible on the happy
> path** — the header pill and the Dashboard engine card are gone, and the
> status footer is empty when the engine is healthy. Engine state surfaces
> in the chrome only when something needs the user's attention:
>
> - **Failed** engine → a red recovery card appears at the top of the
>   layout (above any page) with a "Restart engine" button.
> - **Version mismatch** → a yellow banner above the page content.
>
> §1 (engine lifecycle), §2 (principals), §8 (settings), and §12.2 (crash
> recovery) are rewritten below to reflect v0.3/v0.4.

This checklist exercises **peko-desktop** (the Tauri/React UI) against **peko-runtime** (the daemon) end-to-end. Work through it top-to-bottom. Every row has an **Expected** column — if what you see differs, mark the test **Fail**, choose a **Severity**, and write a short **Note** describing what actually happened.

---

## A. How to use this checklist

For each step:

1. Read the **Step** (what to do) and **Expected** (what should happen).
2. Mark **one** of:
   - **☐ Pass** — saw exactly what was expected
   - **☐ Fail** — saw something different
   - **☐ Skip** — couldn't run this step (write why in Notes)
3. If you marked **Fail**, also tick a **Severity**:
   - **B** — Blocker (can't use the product at all)
   - **M** — Major (feature broken, workaround unclear)
   - **m** — Minor (feature works but with a glitch)
   - **C** — Cosmetic (typo / visual nit)
4. In **Notes**, briefly describe what you actually saw. One short sentence is enough.

When you're done, fill in the **Summary** at the end.

---

## B. Tester & environment

Please fill in before you start:

| Field | Value |
|---|---|
| Tester name | |
| Date (YYYY-MM-DD) | |
| OS + version | (e.g. macOS 15.5, Windows 11 23H2, Ubuntu 24.04) |
| `peko --version` | (run `peko --version` in terminal) |
| peko-desktop version | (see Settings → About in the app) |
| LLM provider(s) tested | (e.g. OpenAI gpt-4o-mini, Anthropic claude-sonnet-4-6) |
| Daemon bind address | (default `127.0.0.1:11435` — only note if changed) |
| Anything else relevant | (recent changes, workarounds used, etc.) |

---

## C. Severity legend

| Code | Label | Meaning | Example |
|---|---|---|---|
| **B** | Blocker | Can't use the product at all | Daemon won't start; app crashes on launch |
| **M** | Major | A core feature is broken, no clear workaround | Chat doesn't stream; peer memory leaks across users |
| **m** | Minor | Feature works but with a noticeable glitch | Wrong timestamp on activity log; markdown bullets don't render |
| **C** | Cosmetic | Visual / typo / polish | Misaligned button; wrong label |

---

## 0. Pre-flight setup (one-time, ~10 min)

Run these once before starting the tests below.

| # | Step | Expected | Result | Severity | Notes |
|---|---|---|---|---|---|
| T-001 | From `peko-runtime/`: `cargo build --release` | Build succeeds, binary at `./target/release/peko` | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |
| T-002 | Move `peko` onto your PATH (or export it) | `which peko` prints a path | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | CLI only — used by T-003..T-005 for provider/credential setup |
| T-003 | `peko provider add --template openai` (or anthropic/kimi/etc.) | Provider added, no error | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |
| T-004 | `peko credential set openai` (paste API key when prompted) | No error; key stored | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |
| T-005 | `peko credential test openai` | Prints success (✓ / ok) | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |
| T-006 | From `peko-desktop/`: `pnpm install` | Install completes | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |
| T-007 | From `peko-desktop/`: `pnpm run sidecar:build-and-fetch` | Rebuilds the runtime in release mode (incremental — fast on a warm cache) and copies the binary to `src-tauri/binaries/peko-<host-triple>`. The script ends by running `peko version` against the copy to confirm the version line lands on stderr. **No separate `peko daemon start` is needed** — the desktop's sidecar supervisor will own the engine (or adopt one that's already on the IPC socket) when you launch the app in T-008. | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | The host triple is detected from `rustc -vV`; on Apple Silicon Macs it's `aarch64-apple-darwin`, on Intel Macs `x86_64-apple-darwin`. The release process handles this in CI; this manual step exists so a local checkout doesn't ship the stub script (`PEKO_VERSION=0.0.0-stub`) into the sidecar slot — that would fail T-103 with a "version mismatch" banner. |
| T-008 | `pnpm tauri dev` (first run is slow — 5–10 min) | App window opens, shows Dashboard | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |

---

## 0a. Reset to first-run state (run only when reproducing)

> **First-time testers can skip this section.** Run these steps only if you've used peko on this machine before, or if you hit a failure mid-test and want to start over from a clean install.
>
> The commands back up any existing state to `<original>.bak.<unix-timestamp>` so you can roll back if needed.

Peko stores state in three places plus the OS keychain. Defaults:

| What | Default location (macOS) | Default location (Linux) |
|---|---|---|
| Config dir (includes `vault.enc`, `providers.toml`) | `~/.peko` | `~/.peko` |
| Data dir (agents, sessions, principals, etc.) | `~/Library/Application Support/peko` | `~/.local/share/peko` |
| Cache dir | `~/Library/Caches/peko` | `~/.cache/peko` |
| Vault DEK in keychain | service `peko`, account `vault-key` (Keychain Access.app) | service `peko`, account `vault-key` (GNOME Keyring / KWallet via `libsecret`) |
| Vault unlock env vars (in your shell) | `PEKO_UNLOCK_METHOD`, `PEKO_MASTER_PASSPHRASE` | same |

If you set `PEKO_HOME`, `PEKO_CONFIG_DIR`, or `PEKO_DATA_DIR` earlier, swap those paths in instead of the defaults above.

| # | Step | Expected | Result | Severity | Notes |
|---|---|---|---|---|---|
| T-009 | **Close the desktop app.** The engine is owned by the sidecar supervisor and stops automatically when the window closes — no manual `peko daemon stop` is needed (or useful). If you previously had a separately-installed `peko` running on PATH, kill that too: `pkill -f "peko daemon"` (or `taskkill /F /IM peko.exe` on Windows). | No peko processes running (verify with `pgrep -f "peko daemon"`) | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |
| T-010 | Unset vault env vars if you have them exported: `unset PEKO_UNLOCK_METHOD PEKO_MASTER_PASSPHRASE`. Verify with `env \| grep PEKO_` showing no output | No output | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |
| T-011 | **(macOS)** Run: `mv ~/.peko ~/.peko.bak.$(date +%s); mv ~/Library/Application\ Support/peko ~/Library/Application\ Support/peko.bak.$(date +%s); mv ~/Library/Caches/peko ~/Library/Caches/peko.bak.$(date +%s); security delete-generic-password -s peko -a vault-key 2>/dev/null; true` | No error; original dirs are gone (backed up) | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |
| T-012 | **(Linux)** Run: `mv ~/.peko ~/.peko.bak.$(date +%s); mv ~/.local/share/peko ~/.local/share/peko.bak.$(date +%s); mv ~/.cache/peko ~/.cache/peko.bak.$(date +%s); (secret-tool clear service peko account vault-key 2>/dev/null \|\| true)` | No error; original dirs are gone (backed up) | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | If `secret-tool` isn't installed, clear via your desktop keyring app (Seahorse / KWallet) — search for service `peko`, account `vault-key` |
| T-013 | Confirm clean state: `ls ~/.peko/vault.enc 2>&1 \| grep -q 'No such' && echo CLEAN` | Prints `CLEAN` | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |
| T-014 | (Optional) Restart the desktop app: re-run T-008 | App re-opens with empty state | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |

> ⚠️ **Why this matters:** without resetting, a vault created in a previous session may live in a different mode (keychain vs. passphrase) than what your current `PEKO_UNLOCK_METHOD` requests, producing `PEKO_UNLOCK_METHOD=… does not match the vault's current mode` — the exact failure mode this test plan avoids by assuming first-run conditions.

---

## 1. Engine lifecycle (happy-path invisibility)

> The engine is the bundled `peko` sidecar owned by the desktop's
> supervisor (ADR-043). It starts when the app launches, follows its own
> lifecycle, and shuts down when you close the window. There are no
> Start/Stop/Restart controls in the UI. As of v0.3 the engine is also
> **invisible on the happy path** — the header pill and the Dashboard
> engine card are gone, and the status footer is empty when the engine
> is healthy. Engine state surfaces in the chrome only when something
> needs the user's attention (Failed, or version mismatch).

| # | Step | Expected | Result | Severity | Notes |
|---|---|---|---|---|---|
| T-101 | With the app closed, run `pgrep -f "peko daemon"` (or `tasklist /FI "IMAGENAME eq peko.exe"` on Windows). Confirm no engine processes are running. | No matching processes | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | Proves we're starting from a clean slate |
| T-102 | Launch the desktop (`pnpm tauri dev` or the bundled app) | App window opens; engine shows up in the process list within ~2 seconds. **The header has no engine pill. The Dashboard has no engine card. The status footer is empty.** | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |
| T-103 | Look at the header, the Dashboard, the status footer, and the Settings page tabs | None of them mention "engine" or "daemon". The header has the theme toggle. The Dashboard shows Principals / Extensions / Quick Actions. Settings has tabs for **General / Credentials / Runtimes / About** only — no Daemon tab. | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | If any of those surfaces shows an engine state badge or pill on a healthy run, or if Settings exposes a Daemon tab, mark **M** — happy-path invisibility is the v0.3 contract. |
| T-104 | **Owned-engine close path** — close the desktop window (X button / `⌘W` / `Alt+F4`). | Engine process exits within a few seconds. Re-run the `pgrep` from T-101 and confirm it's gone. No `zombie peko process holding the lockfile` left behind. | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | This path applies when the desktop spawned its own sidecar (the path T-101 → T-102 takes). For the **borrowed-engine** close path (desktop adopted a CLI daemon), see T-107a — the engine is expected to *survive* the desktop close, by design. |
| T-105 | **First-run walkthrough (T-105)** — assuming a fresh profile (no principals, no credentials, `peko.onboarding.seen` not set in localStorage — see T-009..T-013 for the reset recipe), launch the desktop. | The **First-run walkthrough** overlay auto-appears full-screen above any route. It shows four steps: **(1) pick provider**, **(2) paste API key**, **(3) test credential**, **(4) create principal**. Each step has its own Skip control and a top-level "Skip for now" closes the overlay without creating anything. | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | If the overlay does NOT appear on a fresh profile, or if any of the four steps is missing, mark **M**. After T-105 the localStorage flag `peko.onboarding.seen = "1"` is set even when the user skipped — the "Replay onboarding" escape hatch lives in Settings → About. |
| T-105a | Look at the two stat cards on the Dashboard (now reachable by clicking the Dashboard nav rail item — the walkthrough overlay is dismissed after T-105's last step or Skip) | **Principals: N** and **Extensions: M** — both real numbers (≥ 0), not "undefined" | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |
| T-107 | **Adoption** — with the desktop closed, start a CLI daemon: `peko daemon start`. Then launch the desktop. | App opens. **Nothing in the chrome changes** — still no header pill, no Dashboard card, status footer empty. The engine's "borrowed from CLI daemon" state is **not surfaced** to the desktop user (T-103 invisibility contract — there is no Settings → Daemon tab). | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | If any chrome element claims the engine is "running" or "stopped" without telling the user whose process it is, mark **m** — that's a regression of the invisibility rule. |
| T-107a | With the desktop open and a CLI daemon borrowed (state from T-107), close the desktop. Re-run `pgrep -f "peko daemon"`. | The CLI daemon is still alive. | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | Adoption is mirror-only — the desktop must not stop a process it did not start. |
| T-107b | Reopen the desktop. | Chat works. No engine chrome appears. | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | Reopening the desktop after the CLI daemon survives confirms adoption round-trips cleanly. |
| T-107c | **Liveness** — with the desktop open and a CLI daemon borrowed, in another terminal find the CLI daemon's PID (`pgrep -f "peko daemon"`) and `kill -9 <pid>`. | Within ~5 seconds a red **Engine couldn't start** recovery card appears at the top of the layout (above any page) with a **Restart engine** button. The status footer turns red. Click **Restart engine** on that card. Within ~5 seconds the card disappears, the status footer is empty again, and chat works. The desktop has now spawned its own sidecar (owns_process = true). | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | This is the worst-case "borrowed daemon dies" path — proves the liveness poll + supervisor recovery works. |
| T-108a | **CLI awareness (headless)** — with the desktop closed, run `peko daemon start`. Then run `peko daemon start` again. | The second `peko daemon start` prints something like `⚠️  Daemon is already running (owned by headless daemon, version X)`. | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |
| T-108b | **CLI awareness (sidecar)** — stop the daemon (`peko daemon stop`), launch the desktop (`pnpm tauri dev`), and with the desktop open run `peko daemon start`. | It prints `⚠️  Daemon is already running (owned by sidecar daemon, version X)`. | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | The `mode` field is new in v0.3 — if the warning is missing or doesn't include the mode, the runtime wasn't rebuilt against this PR. |

---

## 2. Principals (sidebar & lifecycle)

A **Principal** is a long-lived AI assistant — it owns its memory, identity, and settings. Desktop users create one in-app via the **Create a principal** modal (Dashboard → New Principal, sidebar empty-state CTA, or Chat empty-state). The CLI (`peko principal new <name> …`) is still supported for automation.

| # | Step | Expected | Result | Severity | Notes |
|---|---|---|---|---|---|
| T-201 | From the Dashboard, click **New Principal** (or open the sidebar empty-state CTA "Create your first principal" if the list is empty). Fill **Name** = `alice`, optionally a description, optionally pick a **provider** pill, optionally pick a **model** from the dropdown that appears, and click **Create**. | Modal closes. The new `alice` principal appears in the sidebar without a manual refresh. The wire path is `principalCreate()` (api.ts) → `principal_create` IPC (peko-runtime PR #185) → `PrincipalManager::create`. | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | **Replaces the old CLI invocation** — desktop users should not need the CLI for principal creation. The model dropdown only appears after a provider is selected and only lists models exposed by that provider's catalog entry. |
| T-201a | **CLI regression** — open a terminal and run `peko principal new bob --provider openai --model gpt-4o-mini --description "CLI regression"` | Bob is created; appears in the sidebar after a refresh | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | This is the CLI path that the desktop modal replaced; keep it green for automation users. |
| T-201b | **CLI per-message override regression** — with `bob` selected, run `peko send bob "Say the model name" --provider openai --model gpt-4o-mini --no-stream` | Command succeeds and the response reflects the requested provider/model. The principal's stored defaults are unchanged. | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | Validates RP8 `--provider` / `--model` wiring. |
| T-202 | In the desktop, refresh the sidebar (or switch pages and back) | Sidebar lists **alice** with a bot icon and a green dot (local, connected) | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |
| T-203 | Click **alice** in the sidebar | Main panel navigates to **Chat** for alice (URL becomes `/chat/alice`) | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |
| T-204 | Type `ali` in the **Search principals…** box | Only alice remains in the list | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |
| T-205 | Clear the search box | Full list returns | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |
| T-205a | **Duplicate-name error path** — try to create another principal named `alice` via the modal. | Modal surfaces an inline error pill (e.g. "principal alice already exists"). No row is added to the sidebar. | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | Proves the runtime's `Manager::create` → `AlreadyExists` → `ResponsePacket::Error` plumbing surfaces cleanly without crashing. |
| T-206 | Right-click on alice in the sidebar | Context menu appears with **Open Chat** and **View Activity** | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |
| T-207 | Click **View Activity** from the menu | Navigates to `/log/alice`, shows an empty-state ("No events yet" or similar) | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |
| T-208 | Go back to Chat via the sidebar | Returns to `/chat/alice` | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |
| T-209 | Hover the green dot next to alice's row | Tooltip says something like "local — connected" | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |

---

## 3. Chat (streaming, markdown, multi-turn, errors)

### 3.1 First send (happy path)

| # | Step | Expected | Result | Severity | Notes |
|---|---|---|---|---|---|
| T-301 | On `/chat/alice`, type `Hello, what can you do?` and click **Send** | Your message appears as a user bubble | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |
| T-302 | Watch the reply | Reply streams in word-by-word within a few seconds | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |
| T-303 | While streaming | Send button is disabled; user can't double-send | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |
| T-304 | When streaming finishes | Full reply visible; send button becomes clickable again | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |

### 3.2 Markdown rendering

| # | Step | Expected | Result | Severity | Notes |
|---|---|---|---|---|---|
| T-305 | Send `Write a 3-item grocery list as markdown bullets` | Reply shows actual bullets (•), not raw `- ` text | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |
| T-306 | Send a fenced code block request (e.g. `Show me a fenced ```js hello world``` block`) | Reply shows syntax-highlighted code in a monospace box | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |

### 3.3 Multi-turn continuity (same session)

| # | Step | Expected | Result | Severity | Notes |
|---|---|---|---|---|---|
| T-307 | Send `My favorite color is blue. Just say "ok" once. Remember it.` | Reply acknowledges ("ok" or similar) | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |
| T-308 | Send `What's my favorite color?` (same chat, same caller) | Reply says **blue** | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |

### 3.4 Per-peer memory isolation

> "Peer" = the user identity you're sending as. The desktop defaults to your local user. Each `(Principal, peer)` pair has its own private conversation thread.

| # | Step | Expected | Result | Severity | Notes |
|---|---|---|---|---|---|
| T-309 | In Settings, find the **Caller Subject** / **User** field. Set it to `alice@example.com`, save | Value persists after switching pages | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |
| T-310 | Back on `/chat/alice`, send `My favorite animal is a platypus. Just say "ok".` | Reply says "ok" | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |
| T-311 | Change Caller Subject to `bob@example.com` | New value shown | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |
| T-312 | In the same chat, send `What's my favorite animal?` | Reply: "I don't know" (NOT platypus — bob should have no memory) | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |
| T-313 | Switch Caller Subject back to `alice@example.com`, re-ask the same | Reply: **platypus** (alice still remembers) | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |

> ⚠️ If T-312 shows "platypus", that's a **Blocker** — peer privacy is broken. Mark it **B** and note what you saw.

### 3.5 Error handling

| # | Step | Expected | Result | Severity | Notes |
|---|---|---|---|---|---|
| T-314 | Settings → **Credentials** → expand your provider card → click **Delete** on a credential row and confirm | Credential removed (no error) | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |
| T-315 | Try to send a chat message | Reply area shows an error chip ("no API key" / similar); app does NOT crash | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |
| T-316 | Re-set the credential in Settings → **Credentials** → expand the provider card → **Add credential** → paste key → **Save** | Saved without error | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |
| T-317 | Send another chat message | Reply streams normally | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |

---

## 4. Principal Log (Activity)

| # | Step | Expected | Result | Severity | Notes |
|---|---|---|---|---|---|
| T-401 | Navigate to `/log/alice` after at least one chat turn | Colored event rows appear chronologically: message (green), tool_call/tool_result (amber/grey), thinking (dashed grey) | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |
| T-402 | Scroll the list | Latest events visible; no infinite spinner | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |
| T-403 | Force a long context: paste a big block, then chat for several turns | A yellow **"context compacted"** row appears at the point of compaction | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |
| T-404 | Check timestamps | Times look correct for your timezone | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |

---

## 5. Cron (scheduled tasks)

> A cron job sends a scheduled message to a principal. Example: every 2 minutes, ask alice to say "pong".

| # | Step | Expected | Result | Severity | Notes |
|---|---|---|---|---|---|
| T-501 | Open the **Cron** page from the rail | Page loads (empty list is fine) | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |
| T-502 | Click **Add** → name=`ping`, schedule=`*/2 * * * *`, message=`Say "pong" and nothing else.` → save | New job appears in the list, status enabled | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |
| T-503 | Wait ~2 minutes, refresh the Cron page | Job's **last run** timestamp updates | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |
| T-504 | Open `/chat/alice` and look for a "pong" message from alice around the run time | "pong" appears in the chat (sent by alice, not by you) | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |
| T-505 | Click **Run now** on the job | A new "pong" arrives in chat within seconds | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |
| T-506 | Click **Delete** on the job | Job disappears from the list | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |
| T-507 | Wait 3 more minutes; check chat | No new "pong" messages | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |

---

## 6. Extensions

| # | Step | Expected | Result | Severity | Notes |
|---|---|---|---|---|---|
| T-601 | Open the **Extensions** page | Lists installed extensions with type / status / hooks columns | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |
| T-602 | If empty, install a simple skill or universal tool you trust | New entry appears in the list | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |

---

## 7. Registry (PekoHub)

Skip this section if PekoHub login isn't configured.

| # | Step | Expected | Result | Severity | Notes |
|---|---|---|---|---|---|
| T-701 | Settings → Runtimes / Registry, log in with your hub token | Status flips to **authenticated** | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |
| T-702 | Open the **Registry** page, search for `alice` or any name | Result cards render with description + author | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |
| T-703 | Click a result's **Pull** button | A new principal appears in the sidebar after a refresh | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |

---

## 8. Settings

### 8.1 Credentials tab

> The Credentials tab is now an accordion of provider cards (RP6). Each
> card shows the provider's display name, API type, enabled toggle,
> default-provider star, edit pencil, and remove trash. Expanding a card
> reveals the credentials stored under `provider:<id>`, a small form to
> add another credential, and a rotation-binding panel for the provider's
> default slot. Orphaned vault keys (credentials whose provider id no
> longer matches the catalog) are surfaced below the accordion.

| # | Step | Expected | Result | Severity | Notes |
|---|---|---|---|---|---|
| T-801 | Open Settings → **Credentials** | A list of provider cards appears (one per catalog entry). Built-in providers are shown even if they have no credentials yet. | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |
| T-801a | If the list is empty, verify the empty-state message | A "No providers available" or equivalent empty state renders, with a button to open the Add Provider modal. | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |
| T-802 | Click a provider card (or its chevron) to expand it | Expanded body shows: (1) existing credentials under that provider, (2) an **Add credential** form, (3) a **Rotation binding** panel. | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | If the provider has no models or no credentials, the relevant sections still render cleanly. |
| T-803 | In the **Add credential** form, enter a name and API key, then click **Save** | A new credential row appears under the provider. The key value is **masked** (•) — clear-text leak would be a security bug. | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |
| T-804 | Click **Test** on the newly added credential | Success indicator (green ✓ / "ok") | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |
| T-805 | Replace the key with a deliberately wrong one, click **Test** | Failure indicator (red ✗ / "auth failed") | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |
| T-806 | Click **Delete** on a credential row and confirm | Row disappears; no error | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |
| T-807 | Click the **Enabled** toggle on a provider card | Toggle flips; provider state is persisted (expand the card again or refresh to confirm). | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |
| T-808 | Click the **Set as default** star on a provider card | The star highlights; other providers' stars are no longer highlighted. The runtime's default provider has changed. | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |
| T-808a | Click the **Edit** pencil on a provider card, change a field (e.g. display name), and save. | The **Edit Provider** modal opens pre-filled; after saving, the card reflects the change without a manual refresh. | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |
| T-808b | Click the **Remove** trash on a custom-added provider and confirm | Provider card disappears from the accordion. | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | Built-in providers may not be removable; that's acceptable. |
| T-809 | In the **Rotation binding** panel of an expanded provider, enter two comma-separated credential ids and click **Save binding** | Binding saved; panel shows the saved order. | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |
| T-810 | Click **Test rotation** | Success indicator (the bound credentials are exercised). | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |
| T-811 | Click **Delete** in the rotation binding panel | Binding removed; panel returns to the empty form. | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |
| T-812 | (Regression) create a credential with a typo'd provider namespace (e.g. via CLI `peko credential set miniax`) | The Credentials tab shows an **Orphaned vault keys** strip below the accordion with the typo'd provider id. | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |

### 8.2 Runtimes tab

| # | Step | Expected | Result | Severity | Notes |
|---|---|---|---|---|---|
| T-821 | Settings → **Runtimes** | At least one runtime listed: **local** with status `connected` | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |
| T-822 | If you have a remote PekoHub runtime added, click **Reconnect** | Status goes `connecting` → `connected` | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |

### 8.3 General / About

> The engine is invisible on the happy path. There is no **Daemon** tab in
> Settings, and no exposed engine diagnostics panel. The only engine chrome
> is the red **Engine couldn't start** recovery card that appears at the
> top of the layout when the supervisor gives up. Engine details (PID,
> lockfile, etc.) are intentionally not surfaced to end users.

| # | Step | Expected | Result | Severity | Notes |
|---|---|---|---|---|---|
| T-831 | Settings → **General** | Form renders, values save when edited. No engine-related controls are present. | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |
| T-832 | Settings → **About** | Shows peko-desktop version + peko-runtime version (real numbers, not "undefined") | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |
| T-833 | **Replay onboarding** — Settings → **About** has a **Replay onboarding** button (or equivalent). Click it. | The First-run walkthrough overlay re-opens. Closing it (Skip or complete) does NOT cause it to auto-reappear again unless the button is clicked again. | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | Escape hatch for the localStorage flag set by T-105. Without this, a tester who clicked Skip during T-105 cannot re-test the walkthrough without clearing localStorage by hand. |

---

## 9. Event Bus

| # | Step | Expected | Result | Severity | Notes |
|---|---|---|---|---|---|
| T-901 | Open the **Event Bus** page | Lists active subscriptions or shows an empty state with a subscribe form | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |
| T-902 | If a subscribe form exists, add a topic like `principal.*`, send a chat message, refresh | New events from your chat appear | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |

---

## 10. Daemon Logs

| # | Step | Expected | Result | Severity | Notes |
|---|---|---|---|---|---|
| T-1001 | Open **Daemon Log** (rail icon or Dashboard link) | Scrollable log of daemon lines renders | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |
| T-1002 | Send a chat message, return to Daemon Logs | New log lines mentioning the principal / send event appear | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |

---

## 11. System health

| # | Step | Expected | Result | Severity | Notes |
|---|---|---|---|---|---|
| T-1101 | From Dashboard or terminal, run **System → Doctor** (or `peko system doctor`) | All checks pass | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |
| T-1102 | Run **System → Clean** (or `peko system clean`) | Cache cleared; app still works afterwards | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |

---

## 12. Cross-cutting behavior

### 12.1 Streaming health

| # | Step | Expected | Result | Severity | Notes |
|---|---|---|---|---|---|
| T-1201 | Send a prompt that produces a long reply (~500+ tokens) | Reply streams smoothly, finishes cleanly without hanging | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |
| T-1202 | Send 5 messages back-to-back quickly | All 5 eventually resolve (queued or parallel) | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |

### 12.2 Engine crash recovery (sidecar supervisor)

> The supervisor owns the engine process. There is no `peko daemon stop` that
> can usefully be run from the terminal — the desktop owns the bundled
> binary, not the one on your PATH. To exercise crash recovery, find the
> sidecar's process ID with `pgrep -f "peko daemon"` and kill it directly.
> In v0.3 there is no header pill, Dashboard card, or Settings → Daemon
> diagnostics panel — the supervisor restarts in the background and the
> chrome stays empty on success. A **Failed** state surfaces as a
> layout-level recovery card (the only place the engine is user-visible in
> the chrome).

| # | Step | Expected | Result | Severity | Notes |
|---|---|---|---|---|---|
| T-1203 | With the desktop open, find the engine PID (`pgrep -f "peko daemon"`). In a terminal: `kill -9 <pid>` (or `taskkill /F /PID <pid>` on Windows). | The chrome stays quiet (no header pill, no Dashboard card). Within ~2 seconds the supervisor's liveness loop restarts the engine. The status footer stays empty. A new chat send still works. | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | The supervisor gives the engine exactly one auto-restart attempt before giving up. If any engine chrome appears during recovery, mark **m** — v0.3 removed those surfaces. |
| T-1204 | Kill the engine a second time within 30 seconds of the first kill. | A red **Engine couldn't start** recovery card appears at the top of the layout (above any page) with a **Restart engine** button. The status footer turns red. Subsequent chat sends fail gracefully with an error chip — the app does NOT crash. | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | Two fails in a row = supervisor gives up. This is intentional: a misconfigured box should not spin the CPU. The recovery card is the v0.3 user-visible surface for a `Failed` engine. |
| T-1204a | In the **Failed** state, click **Restart engine** on the layout-level recovery card. | Card flips through `Restarting…` back to `Running` within ~5 seconds. The recovery card disappears, the status footer is empty again, chat works. | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | This is the supported way out of a **Failed** state without closing the desktop. |
| T-1204b | Close and reopen the desktop (X then relaunch). | On relaunch the engine comes up fresh. The header has no engine pill, the Dashboard has no engine card, the status footer is empty. The Failed recovery card is gone. | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | The supported recovery path for end users. |
| T-1204c | (Optional, advanced) Force a version mismatch by editing the desktop's bundled `binaries/peko-<triple>` symlink to point at a deliberately mismatched runtime (e.g. `ln -sf /tmp/fake-peko binaries/peko-<triple>` where `/tmp/fake-peko` writes `PEKO_VERSION=99.0.0` to stderr). Restart the desktop. | On startup the engine reports a version that doesn't match the desktop's expected version. The header and Dashboard stay quiet (no engine chrome on the happy path); a yellow **Engine version mismatch** banner appears above the page content describing both versions. | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | Reinstalling the desktop fixes the mismatch — the release process guarantees they stay in lockstep. |

### 12.3 Privacy boundary (ADR-042)

> The runtime intentionally does not expose a top-level "sessions" list. Each peer's thread is private.

| # | Step | Expected | Result | Severity | Notes |
|---|---|---|---|---|---|
| T-1205 | Look around the desktop for any "Sessions" page, list, or dropdown | You should **not** find one. (Per-principal Activity is OK — that's `peko log`.) | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |

> ⚠️ If T-1205 finds a Sessions page that lists other users' conversations, mark **B** (Blocker) and capture a screenshot.

### 12.4 Portable Principal export / import

| # | Step | Expected | Result | Severity | Notes |
|---|---|---|---|---|---|
| T-1206 | Run `peko principal export alice --output /tmp/alice.principal` | `/tmp/alice.principal` exists; no error | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |
| T-1207 | Run `peko principal import /tmp/alice.principal --name alice-copy` | Command succeeds | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |
| T-1208 | Refresh the desktop sidebar | **alice-copy** appears in the sidebar | ☐ Pass ☐ Fail | ☐B ☐M ☐m ☐C | |

---

## D. Summary (fill in at the end)

### Counts

| Outcome | Count |
|---|---|
| Total steps run | |
| Pass | |
| Fail | |
| Skipped (with reason) | |

### Severity breakdown of failures

| Severity | Count |
|---|---|
| **B** Blocker | |
| **M** Major | |
| **m** Minor | |
| **C** Cosmetic | |

### Blockers (must-fix before next release)

> List the test IDs and a one-line description for anything marked **B**:

1.
2.
3.

### Top issues observed

> Anything else worth flagging — even passes that had odd moments:

1.
2.
3.

### Tester signature

| Field | Value |
|---|---|
| Tester name | |
| Date completed | |
| Time spent (approx.) | |
| Sign-off | (mark here once summary above is complete: ☐) |

---

## E. How to submit feedback

1. Save this file (don't clear your checkboxes / notes — they're the report).
2. Commit it to a branch or attach it to a ticket.
3. Tag the engineering owner with a link.
4. **Blockers** should be filed as individual tickets with the test ID in the title (e.g. `T-312: peer memory leaks across users`).

---

*End of test plan. Questions → ping the engineering owner listed at the top.*