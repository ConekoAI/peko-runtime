//! Regression guard: streaming must terminate on the canonical end-of-stream
//! signal for each provider format, not wait for the HTTP connection to close.
//!
//! Two providers exhibit the same keep-alive-after-end behaviour:
//!
//! - **OpenAI-style** ends with `data: [DONE]`. Some providers send the
//!   sentinel but then hold the HTTP connection open (keep-alive) instead of
//!   closing the byte stream.
//! - **Anthropic-style** ends with a `message_stop` SSE event. The adapter
//!   maps that to `StreamEvent::Done`; some providers then keep-alive the
//!   connection.
//!
//! `Provider::stream_with_tools` used to loop until `sse_stream.next()`
//! returned `None` — i.e. until the connection closed — so against either
//! kind of provider it blocked forever. That stalled the agentic loop, which
//! stalled the daemon's streaming handler, which hung `peko send` after the
//! final token (the CLI never received `Done` and never exited).
//!
//! These tests stand up a raw TCP server that speaks just enough HTTP/1.1 to
//! stream a chunked SSE response ending in the format's canonical end signal,
//! then deliberately keeps the socket open. With the fix, `stream_with_tools`
//! returns a stream that ends promptly after the end signal; without it,
//! collecting the stream hangs and the wrapping timeout fires. See
//! `src/providers/core.rs`.
#![cfg(unix)]

use futures::StreamExt;
use peko::providers::{
    AnthropicAdapter, AnyAdapter, ChatOptions, LlmMessage, OpenAiAdapter, Provider,
    ProviderRuntimeOptions, StreamEvent,
};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Encode a single SSE `data:` event as one HTTP chunked-transfer chunk.
fn sse_chunk(data: &str) -> Vec<u8> {
    let payload = format!("data: {data}\n\n");
    // chunk = <hex length>\r\n<payload>\r\n
    format!("{:x}\r\n{}\r\n", payload.len(), payload).into_bytes()
}

/// Encode a full SSE event (with optional `event:` and `data:` lines) as one
/// HTTP chunked-transfer chunk. The payload must already end with the
/// terminating blank line.
fn sse_raw_chunk(payload: &str) -> Vec<u8> {
    format!("{:x}\r\n{}\r\n", payload.len(), payload).into_bytes()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stream_terminates_on_done_even_if_connection_stays_open() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");

    // Fake provider: reply with a chunked SSE stream that ends in `[DONE]`,
    // then HOLD the connection open (never send the terminating 0-length
    // chunk, never close). This reproduces the keep-alive-after-DONE
    // behaviour that used to hang the client forever.
    let server = tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.expect("accept");

        // Drain the request headers so reqwest's write side completes.
        let mut buf = Vec::new();
        let mut tmp = [0u8; 1024];
        loop {
            let n = sock.read(&mut tmp).await.expect("read request");
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
            if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }

        sock.write_all(
            b"HTTP/1.1 200 OK\r\n\
              Content-Type: text/event-stream\r\n\
              Transfer-Encoding: chunked\r\n\
              \r\n",
        )
        .await
        .expect("write headers");

        for event in [
            r#"{"choices":[{"delta":{"content":"Hello"},"finish_reason":null}]}"#,
            r#"{"choices":[{"delta":{"content":" world"},"finish_reason":null}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
            "[DONE]",
        ] {
            sock.write_all(&sse_chunk(event))
                .await
                .expect("write chunk");
            sock.flush().await.expect("flush");
        }

        // Intentionally do NOT send `0\r\n\r\n` and do NOT close. Hold the
        // socket open so the client cannot rely on connection close to
        // terminate the stream.
        tokio::time::sleep(Duration::from_secs(30)).await;
        drop(sock);
    });

    let adapter = AnyAdapter::OpenAi(OpenAiAdapter::new().with_base_url(format!("http://{addr}")));
    let options = ProviderRuntimeOptions {
        default_model_id: "gpt-test".to_string(),
        context_window: None,
        timeout_seconds: 30,
        max_retries: 0,
        retry_delay_ms: 0,
        extra_headers: Vec::new(),
    };
    let provider = Provider::new(adapter, "test-key", options).expect("provider");

    let options = ChatOptions {
        temperature: Some(0.0),
        max_tokens: None,
        api_key: None,
        headers: std::collections::HashMap::new(),
    };
    let messages = vec![LlmMessage::user("hi")];

    let mut stream = provider
        .stream_with_tools("gpt-test", &messages, &[], &options)
        .await
        .expect("open stream");

    // The core assertion: draining the stream must COMPLETE. Before the fix
    // this future never resolves (the SSE forwarder blocks on a socket that
    // never closes), so the timeout fires.
    let collected = tokio::time::timeout(Duration::from_secs(10), async {
        let mut text = String::new();
        let mut saw_done = false;
        while let Some(result) = stream.next().await {
            match result.expect("stream event") {
                StreamEvent::TextDelta { delta, .. } => text.push_str(&delta),
                StreamEvent::Done { .. } => saw_done = true,
                _ => {}
            }
        }
        (text, saw_done)
    })
    .await
    .expect(
        "stream_with_tools did not terminate after `[DONE]` — it is waiting for the connection \
         to close, which hangs `peko send` on keep-alive providers",
    );

    server.abort();

    assert_eq!(collected.0, "Hello world", "streamed text mismatch");
    assert!(
        collected.1,
        "expected a Done event to be forwarded before the stream ended"
    );
}

/// Anthropic-style equivalent: the stream ends with a `message_stop` SSE event
/// (mapped by the adapter to `StreamEvent::Done`). The HTTP connection is held
/// open after, mimicking providers that keep-alive the socket. Before the
/// fix, `stream_with_tools` would never see the byte stream end and would
/// hang — the agentic loop would stall and `peko send` would never exit.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stream_terminates_on_message_stop_even_if_connection_stays_open() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");

    let server = tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.expect("accept");

        let mut buf = Vec::new();
        let mut tmp = [0u8; 1024];
        loop {
            let n = sock.read(&mut tmp).await.expect("read request");
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
            if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }

        sock.write_all(
            b"HTTP/1.1 200 OK\r\n\
              Content-Type: text/event-stream\r\n\
              Transfer-Encoding: chunked\r\n\
              \r\n",
        )
        .await
        .expect("write headers");

        // Minimal Anthropic SSE sequence: a single text chunk followed by
        // `message_stop`. The adapter turns the latter into
        // `StreamEvent::Done`; the HTTP byte stream is *not* closed.
        for event in [
            "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"m\",\"role\":\"assistant\",\"content\":[],\"model\":\"m\",\"stop_reason\":null,\"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}\n\n",
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\n",
            "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n",
            "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
        ] {
            sock.write_all(&sse_raw_chunk(event))
                .await
                .expect("write chunk");
            sock.flush().await.expect("flush");
        }

        // Hold the socket open: no terminating 0-length chunk, no close.
        tokio::time::sleep(Duration::from_secs(30)).await;
        drop(sock);
    });

    let adapter =
        AnyAdapter::Anthropic(AnthropicAdapter::new().with_base_url(format!("http://{addr}")));
    let options = ProviderRuntimeOptions {
        default_model_id: "claude-test".to_string(),
        context_window: None,
        timeout_seconds: 30,
        max_retries: 0,
        retry_delay_ms: 0,
        extra_headers: Vec::new(),
    };
    let provider = Provider::new(adapter, "test-key", options).expect("provider");

    let options = ChatOptions {
        temperature: Some(0.0),
        max_tokens: None,
        api_key: None,
        headers: std::collections::HashMap::new(),
    };
    let messages = vec![LlmMessage::user("hi")];

    let mut stream = provider
        .stream_with_tools("claude-test", &messages, &[], &options)
        .await
        .expect("open stream");

    let collected = tokio::time::timeout(Duration::from_secs(10), async {
        let mut text = String::new();
        let mut saw_done = false;
        while let Some(result) = stream.next().await {
            match result.expect("stream event") {
                StreamEvent::TextDelta { delta, .. } => text.push_str(&delta),
                StreamEvent::Done { .. } => saw_done = true,
                _ => {}
            }
        }
        (text, saw_done)
    })
    .await
    .expect(
        "stream_with_tools did not terminate after message_stop — it is waiting for the \
         connection to close, which hangs `peko send` on keep-alive providers",
    );

    server.abort();

    assert_eq!(collected.0, "Hello", "streamed text mismatch");
    assert!(
        collected.1,
        "expected a Done event (from message_stop) to be forwarded before the stream ended"
    );
}
