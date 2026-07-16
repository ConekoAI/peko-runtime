//! Regression guard: `peko send` must use the STREAMING request path.
//!
//! The one-shot `PrincipalSend` request emits nothing until the whole
//! answer is computed, so (a) output never streams and (b) any response
//! slower than the CLI's per-packet idle timeout dies with "Stream closed
//! unexpectedly". `peko send` therefore issues `PrincipalSendStream`, whose
//! `PrincipalSentChunk` deltas both stream to the terminal and keep the
//! idle timer alive. See `src/commands/send.rs`.
//!
//! Unlike `cli_send.rs`, this test needs no mock LLM and no real daemon: it
//! binds a fake `UnixDatagram` "daemon" on the exact socket path the CLI
//! resolves from `PEKO_DAEMON_SOCK` (set by `PekoCli::cmd`). The fake daemon
//! runs as a serve loop — the CLI pings several times before the send (the
//! transport auto-detect in `init_extension_core` connects and pings too),
//! so the loop answers every `Ping` with `Pong` and, when it sees the send
//! request, records its variant and replies with two chunks plus `Done`. If
//! someone reverts `handle_send` to the one-shot `principal_send`, the
//! recorded variant is `PrincipalSend` and the assertion fails.
//!
//! Unix-only: the fake server speaks the Unix datagram transport. The
//! Windows named-pipe transport is exercised by the gated `cli_send` suite.
#![cfg(unix)]

mod common;
use common::PekoCli;
use peko::ipc::{RequestPacket, ResponsePacket};
use std::process::Stdio;
use std::time::Duration;
use tokio::net::UnixDatagram;

/// What the fake daemon observed for the first send request.
struct Observed {
    /// Variant name of the send request the CLI issued.
    request_variant: String,
    /// The message payload it carried.
    message: String,
}

/// Reply to a send request with two streamed chunks, the final full answer,
/// and `Done` — so the CLI renders and terminates regardless of variant.
async fn reply_send(server: &UnixDatagram, peer_path: &std::path::Path, request_id: u64) {
    for delta in ["Hello, ", "world"] {
        let chunk = ResponsePacket::PrincipalSentChunk {
            request_id,
            delta: delta.into(),
        }
        .to_bytes()
        .expect("encode chunk");
        let _ = server.send_to(&chunk, peer_path).await;
    }
    let done_payload = ResponsePacket::PrincipalSentDone {
        request_id,
        content: "Hello, world".into(),
    }
    .to_bytes()
    .expect("encode done payload");
    let _ = server.send_to(&done_payload, peer_path).await;
    let done = ResponsePacket::Done {
        request_id,
        success: true,
        error: None,
    }
    .to_bytes()
    .expect("encode done");
    let _ = server.send_to(&done, peer_path).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn send_uses_streaming_request_and_renders_chunks() {
    let cli = PekoCli::new();
    let sock_path = cli.daemon_sock();
    std::fs::create_dir_all(sock_path.parent().expect("sock parent")).expect("create run dir");
    let _ = std::fs::remove_file(&sock_path);
    let server = UnixDatagram::bind(&sock_path).expect("bind fake daemon socket");

    // Channel carrying the first observed send request out of the loop.
    let (send_tx, mut send_rx) = tokio::sync::mpsc::channel::<Observed>(1);

    // Fake daemon serve loop: answer every Ping with Pong, and reply to the
    // send request with two chunks + Done so the CLI terminates.
    let server_task = tokio::spawn(async move {
        let mut buf = vec![0u8; 65536];
        let mut send_tx = Some(send_tx);
        loop {
            let (len, peer) = match server.recv_from(&mut buf).await {
                Ok(v) => v,
                Err(_) => break,
            };
            let Some(peer_path) = peer.as_pathname().map(|p| p.to_path_buf()) else {
                continue;
            };
            let Ok(req) = RequestPacket::from_bytes(&buf[..len]) else {
                continue;
            };

            match req {
                RequestPacket::Ping { request_id } => {
                    let pong = ResponsePacket::Pong {
                        request_id,
                        uptime_secs: 0,
                        version: "test".into(),
                    }
                    .to_bytes()
                    .expect("encode pong");
                    let _ = server.send_to(&pong, &peer_path).await;
                }
                RequestPacket::PrincipalSendStream {
                    request_id,
                    message,
                    ..
                } => {
                    reply_send(&server, &peer_path, request_id).await;
                    if let Some(tx) = send_tx.take() {
                        let _ = tx
                            .send(Observed {
                                request_variant: "PrincipalSendStream".to_string(),
                                message,
                            })
                            .await;
                    }
                }
                RequestPacket::PrincipalSend {
                    request_id,
                    message,
                    ..
                } => {
                    reply_send(&server, &peer_path, request_id).await;
                    if let Some(tx) = send_tx.take() {
                        let _ = tx
                            .send(Observed {
                                request_variant: "PrincipalSend".to_string(),
                                message,
                            })
                            .await;
                    }
                }
                _ => {}
            }
        }
    });

    // Run `peko send` against the fake daemon. `cli.cmd()` sets
    // `PEKO_DAEMON_SOCK` to `sock_path`, so no real daemon is needed.
    let mut cmd = tokio::process::Command::from(cli.cmd());
    cmd.args(["send", "test-agent", "ping"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = tokio::time::timeout(Duration::from_secs(20), cmd.output())
        .await
        .expect("peko send timed out")
        .expect("spawn peko send");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    let observed = tokio::time::timeout(Duration::from_secs(5), send_rx.recv())
        .await
        .expect("timed out waiting for the send request")
        .expect("fake daemon never saw a send request");
    server_task.abort();

    // The core regression guard: `send` must use the streaming request.
    assert_eq!(
        observed.request_variant, "PrincipalSendStream",
        "peko send issued {} instead of PrincipalSendStream — the one-shot \
         path defeats streaming and trips the CLI idle timeout on slow \
         responses.\nstdout: {stdout}\nstderr: {stderr}",
        observed.request_variant
    );
    assert_eq!(
        observed.message, "ping",
        "the send request did not carry the message payload"
    );

    // And the streamed chunks must be rendered to stdout.
    assert_eq!(
        output.status.code(),
        Some(0),
        "peko send exited non-zero\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("Hello, world"),
        "stdout did not render the streamed chunks 'Hello, world'\n\
         stdout: {stdout}\nstderr: {stderr}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn send_forwards_model_override() {
    let cli = PekoCli::new();
    let sock_path = cli.daemon_sock();
    std::fs::create_dir_all(sock_path.parent().expect("sock parent")).expect("create run dir");
    let _ = std::fs::remove_file(&sock_path);
    let server = UnixDatagram::bind(&sock_path).expect("bind fake daemon socket");

    let (send_tx, mut send_rx) = tokio::sync::mpsc::channel::<(Option<String>, String)>(1);

    let server_task = tokio::spawn(async move {
        let mut buf = vec![0u8; 65536];
        let mut send_tx = Some(send_tx);
        loop {
            let (len, peer) = match server.recv_from(&mut buf).await {
                Ok(v) => v,
                Err(_) => break,
            };
            let Some(peer_path) = peer.as_pathname().map(|p| p.to_path_buf()) else {
                continue;
            };
            let Ok(req) = RequestPacket::from_bytes(&buf[..len]) else {
                continue;
            };

            match req {
                RequestPacket::Ping { request_id } => {
                    let pong = ResponsePacket::Pong {
                        request_id,
                        uptime_secs: 0,
                        version: "test".into(),
                    }
                    .to_bytes()
                    .expect("encode pong");
                    let _ = server.send_to(&pong, &peer_path).await;
                }
                RequestPacket::PrincipalSendStream {
                    request_id,
                    message,
                    override_model,
                    ..
                } => {
                    reply_send(&server, &peer_path, request_id).await;
                    if let Some(tx) = send_tx.take() {
                        let _ = tx.send((override_model, message)).await;
                    }
                }
                _ => {}
            }
        }
    });

    let mut cmd = tokio::process::Command::from(cli.cmd());
    cmd.args([
        "send",
        "test-agent",
        "ping",
        "--model",
        "anthropic-claude-sonnet-4-5",
    ])
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    let output = tokio::time::timeout(Duration::from_secs(20), cmd.output())
        .await
        .expect("peko send timed out")
        .expect("spawn peko send");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    let (override_model, message) = tokio::time::timeout(Duration::from_secs(5), send_rx.recv())
        .await
        .expect("timed out waiting for the send request")
        .expect("fake daemon never saw a send request");
    server_task.abort();

    assert_eq!(message, "ping");
    assert_eq!(
        override_model,
        Some("anthropic-claude-sonnet-4-5".to_string())
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "peko send exited non-zero\nstdout: {stdout}\nstderr: {stderr}"
    );
}
