//! Regression guard: streaming must terminate on the `[DONE]` SSE sentinel,
//! not wait for the HTTP connection to close.
//!
//! Some OpenAI-compatible providers emit `data: [DONE]` to mark the end of a
//! stream but then hold the HTTP connection open (keep-alive) instead of
//! closing the byte stream. `Provider::stream_with_tools` used to loop until
//! `sse_stream.next()` returned `None` — i.e. until the connection closed —
//! so against such a provider it blocked forever. That stalled the agentic
//! loop, which stalled the daemon's streaming handler, which hung `peko send`
//! after the final token (the CLI never received `Done` and never exited).
//!
//! This test stands up a raw TCP server that speaks just enough HTTP/1.1 to
//! stream a chunked SSE response ending in `[DONE]`, then deliberately keeps
//! the socket open. With the fix, `stream_with_tools` returns a stream that
//! ends promptly after `[DONE]`; without it, collecting the stream hangs and
//! the wrapping timeout fires. See `src/providers/core.rs`.
#![cfg(unix)]

use futures::StreamExt;
use peko::providers::{
    AnyAdapter, ChatOptions, LlmMessage, OpenAiAdapter, Provider, ProviderConfig, StreamEvent,
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
            sock.write_all(&sse_chunk(event)).await.expect("write chunk");
            sock.flush().await.expect("flush");
        }

        // Intentionally do NOT send `0\r\n\r\n` and do NOT close. Hold the
        // socket open so the client cannot rely on connection close to
        // terminate the stream.
        tokio::time::sleep(Duration::from_secs(30)).await;
        drop(sock);
    });

    let adapter =
        AnyAdapter::OpenAi(OpenAiAdapter::new().with_base_url(format!("http://{addr}")));
    let config = ProviderConfig::default();
    let provider = Provider::new(adapter, "test-key", config).expect("provider");

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
