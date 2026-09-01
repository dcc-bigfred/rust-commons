# BigFred SDK

Client libraries for [BigFred](https://github.com/dcc-bigfred/bigfred).

| Language | Path | Status |
|---|---|---|
| Rust | [`rust/`](rust/) | `bigfred-client` |
| Go | `go/` | planned |

## Rust — `bigfred-client`

HTTP, OAuth drop-in, reverse-proxy helpers, and dcc-bus WebSocket. No axum: the host owns HTTP handlers and maps errors onto its envelope.

### Git (from `main`)

```toml
[dependencies]
bigfred-client = { git = "https://github.com/dcc-bigfred/sdk.git", branch = "main" }
```

Cargo resolves the crate by package name under `rust/crates/bigfred-client`.

### crates.io

Published on tag `v*` (`cargo publish -p bigfred-client`). Until the first release, use the git dependency above.

```toml
[dependencies]
bigfred-client = "0.1"
```

The crate’s `reqwest` build has no TLS features (loopback HTTP to BigFred on the hub).
