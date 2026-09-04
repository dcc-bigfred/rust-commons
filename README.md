# BigFred Rust commons

Shared modules and architectural patterns for hub daemons written in Rust.

The HTTP / OAuth / dcc-bus **client SDK** lives in [`dcc-bigfred/bigfred`](https://github.com/dcc-bigfred/bigfred) (`rust/crates/bigfred-client`).

| Crate | Path | Role |
| --- | --- | --- |
| `dcc-daemon` | [`rust/crates/dcc-daemon`](rust/crates/dcc-daemon) | `$DATA_DIR` paths, config load + hot-reload, Unix-socket command server (IPC bind is always singleton — see [crate README](rust/crates/dcc-daemon/README.md)) |

## Rust — `dcc-daemon`

```toml
[dependencies]
dcc-daemon = { git = "https://github.com/dcc-bigfred/rust-commons", branch = "main" }
```

Features: `ipc` (framing, bind, `Command` router), `config` (JSON load + inotify watch). Default enables both.

Until the first crates.io release, use the git dependency above.
