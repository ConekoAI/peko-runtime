# ADR-038: Windows Named-Pipe Transport

**Status**: Accepted
**Date**: 2026-06-14
**Last Updated**: 2026-06-14
**Author**: Kimi Code CLI + User
**Depends On**: ADR-021 (Daemon as Central Runtime), ADR-034 (Runtime Authentication and Authorization)
**Supersedes**: The "Windows falls back to UDP" row in ADR-021 §Consequences (line 422) and the corresponding §Architecture paragraph at ADR-021 line 227.

## Context

ADR-021 settled the peko-runtime IPC transport as a Unix domain datagram socket on Unix and UDP-on-localhost on Windows. The Windows compromise was explicit:

> "Windows lacks Unix sockets. Fall back to UDP on Windows. Both use same protocol, just different transport." — ADR-021 line 422

This compromise was acceptable while Windows support was best-effort, but it has three concrete costs:

1. **No file-permission auth.** The Unix path relies on `daemon.sock` being created with mode 0600 — the kernel itself gates "who can talk to the daemon." UDP on Windows has no such boundary; the trust decision reduces to "is the source IP loopback," which any local process can satisfy. ADR-034's `enforce_auth_for_public_bind` only fires for *public* binds, so a Windows user who runs the daemon on `127.0.0.1` has no transport-layer trust at all.
2. **UDP semantics are not a great fit.** UDP is connectionless, unreliable, and datagram-bounded. Loopback UDP rarely drops packets in practice, but the protocol layer still has to handle sequence gaps, heartbeats, and an explicit "trust the application layer" boundary that Unix-domain sockets don't require.
3. **Test coverage fragmentation.** Four integration-test files (`cli_send.rs`, `cli_cron.rs`, `cli_basics.rs`, `cli_session.rs`) carry `#![cfg(unix)]` because the daemon's IPC server is Unix-only. The transport layer — the part most likely to differ between platforms — is the part that never gets tested on Windows in CI.

This ADR replaces the Windows UDP fallback with **Windows named pipes**, the only Windows IPC primitive that gives kernel-enforced peer identity, message-mode framing, and a trust model analogous to Unix 0600. The change is contained to the IPC layer; the wire protocol, request/response types, and 12 internal callers of `DaemonClient` / `ConnectionManager` are unchanged.

## Decision

**On Windows, the peko daemon binds a Windows named pipe as its local IPC transport:**

- **Name:** `\\.\pipe\peko-{username}` by default, where `{username}` is the value of `USERNAME` (with `USER` as a fallback), sanitised to characters valid in a Win32 pipe name (`\`, `?`, `*`, `<`, `>`, `|`, `"` → `_`) and capped at 64 characters. The full pipe path stays under the 256-character `MAX_PATH` limit.
- **Override:** `PEKO_DAEMON_PIPE` environment variable. Mirrors the existing `PEKO_DAEMON_SOCK` and `PEKO_DAEMON_ADDR` overrides.
- **DACL:** `O:BAD:(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;OW)` — owner-only Generic All, plus LocalSystem and Built-in Administrators for parity with the Unix 0600 mode that still permits system/admin tools to read the socket.
- **Pipe mode:** `PipeMode::Message` — one read = one whole message, capped at 64 KB, matches the existing `MAX_PACKET_SIZE` in `src/ipc/packet.rs:14` and avoids a length prefix in the wire framing.
- **Max instances:** 64. Allows multiple concurrent clients per pipe name.
- **First-instance flag:** `true` on bind; `daemon start` refuses if another peko is already bound. Matches the Unix "remove stale socket file, then bind" behavior.

**Discovery ladder (replaces ADR-021 §Daemon Discovery table):**

| Step | Unix | Windows |
|------|------|---------|
| 1 | `PEKO_DAEMON_SOCK` env var (Unix socket) | `PEKO_DAEMON_SOCK` env var (no-op stub) |
| 2 | `PEKO_DAEMON_ADDR` env var (UDP) | `PEKO_DAEMON_ADDR` env var (UDP) |
| 3 | — | `PEKO_DAEMON_PIPE` env var (named pipe) |
| 4 | default Unix socket `~/.peko/run/daemon.sock` | default named pipe `\\.\pipe\peko-{user}` |
| 5 | default UDP `127.0.0.1:11435` | default UDP `127.0.0.1:11435` |

**UDP is retained as the explicit-remote transport and the universal last-resort safety net** — see "UDP Retention" below.

**The `ResponseSink` trait** (`src/ipc/response_sink.rs`) abstracts the per-transport response-write path so the giant `handle_request` match in `server.rs` is platform-agnostic. Unix/UDP build a per-request sink that captures the peer address from `recv_from`; Windows builds a `PipeSink` over a `&mut NamedPipeServer` from the accept loop.

**`enforce_auth_for_public_bind` is gated to the UDP transport only.** Unix sockets and named pipes have their own transport-layer trust boundaries; the function's `&SocketAddr` signature stays unchanged.

## Why Named Pipes on Windows

The user-facing reason: **kernel-enforced peer identity**. `CreateNamedPipeW` with a DACL lets the kernel reject `CreateFileW` from any process whose SID isn't in the ACE. The result is the same trust story we get for free from Unix 0600 sockets: "any process owned by the daemon's user can talk to the daemon, and no one else."

The engineering reasons (mirroring ADR-021 lines 220-226 "Why Unix Domain Sockets"):

- **Reliable, ordered, in-kernel framing.** `PipeMode::Message` gives one read = one whole message, capped at 64 KB. No UDP packet loss concerns, no sequence-gap handling, no "trust the application layer" for ordering.
- **File-permission equivalent.** The pipe's DACL is the analog of Unix 0600 mode. The DACL is consulted by the kernel at `CreateFileW` time; the daemon's application code does not have to re-derive trust.
- **No firewall surface.** Named pipes are local-only; Windows Defender Firewall does not prompt for them. The current UDP fallback has been observed to trigger firewall popups in some configurations; named pipes do not.
- **Standard primitive.** `tokio::net::windows::named_pipe` is a stable, well-documented Tokio surface. The Win32 `CreateNamedPipeW` API has been stable since Windows NT 3.1. There is no third-party dependency to add beyond `windows-sys = "0.52"` (already a project dependency for Job Objects).
- **Test coverage symmetry.** With the gate dropped, the four `#![cfg(unix)]` files (`cli_send.rs`, `cli_cron.rs`, `cli_basics.rs`, `cli_session.rs`) now run on both platforms. CI catches transport-layer regressions that today can only surface on a developer machine.

## Trust Boundary

References ADR-034. The local-trust boundary now has two layers:

1. **Transport layer (this ADR).** The kernel checks the client's SID at `CreateFileW` time. Cross-user connections are rejected with `ERROR_ACCESS_DENIED` before any application code runs.
2. **Application layer (ADR-034).** `CallerContext` resolution via `resolve_caller` continues to honor `peer.is_local()` for the in-process auth dispatch. On Windows the `PeerAddr::Local` variant (a unit) is always local by construction, which preserves the ADR-034 `local_trust` flow.

For *remote* access, the existing `enforce_auth_for_public_bind` path stays in place — but it now only fires for UDP binds. Unix sockets and named pipes cannot bind to a public address; they are inherently local.

## UDP Retention

UDP stays in the IPC stack for two reasons:

1. **Explicit remote transport.** ADR-021's §Out of Scope (line 442) notes "Remote daemon: This is local IPC only. Remote would need TCP + TLS." UDP is the closest thing to that today — a user who runs `peko` in a container and wants the host CLI to talk to it can `bind 0.0.0.0:11435` and ADR-034's `enforce_auth_for_public_bind` gates the bind. Named pipes cannot do this.
2. **Universal safety net.** If the named-pipe bind fails (unusual test environment, sandbox restrictions, antivirus interference) the daemon falls back to UDP, just as Unix falls back to UDP when `UnixDatagram::bind` fails today. This is the same "last resort" pattern, kept for resilience.

The new discovery ladder places UDP as the *fifth* and final step. The vast majority of users will never reach it; it exists for misconfigured environments and the explicit-remote case.

## Implementation

The implementation is contained to the IPC layer (`src/ipc/`), the test harness (`tests/common/cli.rs`, `tests/common/daemon.rs`), the `windows-sys` feature set, and four test-gate removals. No caller of `DaemonClient` / `ConnectionManager` changes.

**Key files:**

- `src/ipc/pipe_security.rs` (new) — Win32 FFI for the DACL, mirrors the Job Object FFI pattern at `src/common/process/job_object.rs`.
- `src/ipc/response_sink.rs` (new) — `ResponseSink` trait + per-transport impls.
- `src/ipc/server.rs` — `ServerSocket::NamedPipe` variant, `IpcServer::new` Windows branch, `run_pipes` / `handle_pipe_connection` helpers, `handle_request` signature takes `&dyn ResponseSink` (replaces `&ServerSocket` + `&PeerAddr`).
- `src/ipc/connection.rs` — `ConnectionHandle::NamedPipe` variant, `connect_pipe_with_timeout`, extended discovery ladder.
- `src/ipc/mod.rs` — `DAEMON_PIPE_ENV` constant, `default_pipe_name()` helper.
- `tests/common/cli.rs` — `PekoCli::daemon_endpoint()` and per-test unique `PEKO_DAEMON_PIPE` on Windows.
- `tests/common/daemon.rs` — panic message uses `daemon_endpoint()`.
- `tests/cli_send.rs`, `tests/cli_cron.rs`, `tests/cli_basics.rs`, `tests/cli_session.rs` — `#![cfg(unix)]` gate removed.

**Dependencies (Cargo.toml):**

- `tokio = { features = ["full", "net"] }` — `net` is enabled by `full` in tokio 1.35 but listed explicitly so the dependency on `tokio::net::windows::named_pipe` is self-documenting.
- `windows-sys = { features = [..., "Win32_System_Pipes", "Win32_Security"] }` — `CreateNamedPipeW` and `ConvertStringSecurityDescriptorToSecurityDescriptorW`.

## Alternatives Considered

1. **Default security (Everyone can connect).** Rejected: weakens the trust story to the same level as the current UDP fallback. The whole point of this ADR is to remove the UDP-fallback compromise; default-Everyone defeats the purpose.
2. **Stick with UDP.** Rejected as a long-term solution. The trust-model compromise, the test-fragmentation cost, and the protocol-layer complexity of running UDP for local IPC are all real and accumulate over time.
3. **AF_UNIX via WSL / WSL2.** Rejected: not generally available (only on machines with WSL installed), and would require WSL-distro-aware configuration. The peko CLI running in WSL would also need to know the host's distro, which is brittle.
4. **TCP / TLS.** Rejected as a local IPC transport. TCP is connection-oriented like named pipes, but TLS handshake overhead, certificate management, and the "is this loopback?" question all push back against using it for the local-trust case. UDP is the explicit-remote transport; TCP would be the cross-host transport and is out of scope per ADR-021 §Out of Scope line 442.
5. **Drop UDP entirely.** Rejected: UDP is the explicit-remote transport and the universal safety net. Removing it loses both use cases.

## Consequences

### Positive

- **Symmetric trust story on Unix and Windows.** File mode 0600 on Unix ↔ named-pipe DACL on Windows. The kernel enforces peer identity on both platforms; the application layer can rely on `peer.is_local()` being meaningful.
- **Test coverage parity.** The four `#![cfg(unix)]` files run on both platforms. Transport-layer regressions are caught in CI on both OSes.
- **No firewall prompts.** Named pipes do not interact with Windows Firewall; the current UDP path has been observed to trigger Defender prompts in some configurations.
- **No caller-code change.** The 12 internal consumers of `DaemonClient` / `ConnectionManager` continue to use the same surface.

### Negative

- **New platform-specific file.** `src/ipc/pipe_security.rs` contains Win32 FFI; the `unsafe` block is scoped to the SDDL conversion and `LocalFree` cleanup. Mirrors the existing Job Object FFI module.
- **Per-connection dispatch model.** Unix datagram sockets are connectionless (one bound socket, many peers via `send_to`); named pipes are connection-oriented (one accept per connection, one `&mut NamedPipeServer` per task). The `ResponseSink` trait factors this difference out so the giant `handle_request` match is unchanged.
- **No Windows CI in this PR.** Adding a Windows CI runner is a follow-up. The change is locally testable on a Windows dev box, and the test-gate removal unlocks symmetric CI in a follow-up PR.
- **One new env var.** `PEKO_DAEMON_PIPE` is added to the discovery ladder. Documentation in `docs/integration/TESTING.md` should follow.

## References

- ADR-021: Daemon as Central Runtime (the Windows-fallback line being superseded)
- ADR-034: Runtime Authentication and Authorization (the `enforce_auth_for_public_bind` function whose call site is now gated to UDP only)
- Microsoft: [Creating a Pipe](https://learn.microsoft.com/en-us/windows/win32/ipc/creating-a-pipe)
- Microsoft: [SDDL Strings](https://learn.microsoft.com/en-us/windows/win32/secauthz/sddl-strings)
- Tokio: [`tokio::net::windows::named_pipe`](https://docs.rs/tokio/latest/tokio/net/windows/named_pipe/index.html)
- Docker daemon: pipe-based IPC on Windows (the architectural analog)
