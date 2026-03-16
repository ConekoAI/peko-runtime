# Pekobot HTTP API

This module implements the HTTP API server for the Pekobot daemon.

## Architecture

The API server is built on [Axum](https://docs.rs/axum) and follows the contract defined in `API_CONTRACT.md`.

```
src/api/
├── mod.rs              # Module exports and constants
├── server.rs           # Axum server setup and lifecycle
├── state.rs            # Shared application state
├── error.rs            # Error types and HTTP conversions
├── types.rs            # Request/response types
├── middleware/         # HTTP middleware
│   ├── version.rs      # X-Pekobot-Version header
│   ├── request_id.rs   # X-Request-ID handling
│   └── logging.rs      # Request logging
└── routes/             # Endpoint handlers
    ├── health.rs       # GET /health
    └── info.rs         # GET /info
```

## Configuration

The server is configured via `DaemonConfig`:

- `host`: Bind address (default: `127.0.0.1`)
- `port`: Port number (default: `11435`)

## Security

- **Localhost-only by default:** The server binds to `127.0.0.1` by default, preventing external network access.
- **Security warning:** Binding to non-loopback addresses (e.g., `0.0.0.0`) logs a prominent security warning.

## Headers

All responses include:

- `X-Pekobot-Version`: Pekobot version string
- `X-Request-ID`: Request correlation ID (echoed from request or auto-generated)

## Error Format

All errors follow the standard envelope:

```json
{
  "error": {
    "code": "error_code",
    "message": "Human readable message",
    "request_id": "uuid-for-tracing",
    "details": {} // Optional additional details
  }
}
```

## Testing

Run the integration tests (requires a running daemon):

```bash
# Start daemon in one terminal
pekobot daemon start --foreground

# Run API tests in another terminal
cargo test --test api_integration_tests -- --ignored
```

Unit tests are inline in each module and can be run with:

```bash
cargo test --lib api
```

## Adding New Endpoints

1. Add handler function in `routes/<resource>.rs`
2. Register route in `routes/mod.rs::create_router()`
3. Add types to `types.rs` if needed
4. Add error variants to `error.rs` if needed
5. Add tests!

Example:

```rust
// routes/agents.rs
pub async fn list_agents(State(state): State<AppState>) -> Response {
    // Implementation
}

// routes/mod.rs
pub mod agents;

pub fn create_router() -> Router<AppState> {
    Router::new()
        .route("/agents", get(agents::list_agents))
        // ... other routes
}
```
