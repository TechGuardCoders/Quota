# Quota

Per-tenant token/request budgets with scheduled reset. Pure Python, zero
dependencies, injectable clock for deterministic tests.

## Results

- `allow(tenant, tokens) -> bool` — consumes budget on success; returns a
  structured `Denial` (falsy, with `retry_after` seconds) when over limit.
  Never raises, never blocks.
- Warn flag at 80% of the limit per tenant.
- Windows reset on rollover; tenants are fully isolated.

## Architecture

```
Quota(limit, window_s, clock)
 ├── _windows: {tenant: _Window(started_at, used)}
 ├── allow(tenant, tokens)   -> bool | Denial
 ├── warn(tenant)            -> bool   (>= 80% used)
 ├── remaining(tenant)       -> int
 └── usage(tenant)           -> int
```

A fixed window is created lazily per tenant and replaced when
`now - started_at >= window_s`. The clock is injectable, so tests advance
time without sleeping and CI never touches the network.

## Why

Rate limiting per tenant is a cross-cutting concern that should fail clean:
a caller over budget gets a machine-readable denial with `retry_after`
(HTTP 429 semantics) instead of an exception or a hang. Keeping it
dependency-free and clock-injectable makes it safe to embed in any service
and trivial to test.

## Usage

```python
from quota import Quota

quota = Quota(limit=100_000, window_s=86_400)

if quota.allow("tenant-a", 500):
    serve()
else:
    denial = quota.allow("tenant-a", 500)   # falsy Denial
    print(denial.retry_after)               # seconds until reset

if quota.warn("tenant-a"):
    log("tenant-a above 80% of budget")
```

## Rust implementation

`src/lib.rs` is a 1:1 port of the Python reference (same semantics, same
test suite — 11 tests in `cargo test`). Zero dependencies, injectable clock
(`Clock` trait), `Decision::Allowed | Denied(Denial)` instead of Python's
truthy/falsy duality, and negative tokens are unrepresentable via `u64`.

```bash
cargo test
```

```rust
use quota::{Quota, Decision};
use std::time::Duration;

let mut quota = Quota::new(100_000, Duration::from_secs(86_400))?;

if let Decision::Denied(d) = quota.allow("tenant-a", 500) {
    eprintln!("denied: retry after {:?}", d.retry_after); // HTTP 429 semantics
}

if quota.warn("tenant-a") {
    log("tenant-a above 80% of budget");
}
```

## Development

```bash
uv venv && . .venv/bin/activate  # or Scripts/activate on Windows
uv pip install pytest
pytest
cargo test   # Rust port
```

## Docker

Multi-stage build on `python:3.12-slim`, non-root, multi-arch (amd64 + arm64):

```bash
docker buildx build --platform linux/amd64,linux/arm64 -t <name> .
```

The image imports the library as a smoke check on run. For repos with a Rust
port (Ledger, Quota, Blackbox), pass `--build-arg BUILD_RUST=1` to also build
the Rust artifact in a `rust:1.83-slim` stage.
