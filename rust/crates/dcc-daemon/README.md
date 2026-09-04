# dcc-daemon

Shared runtime pieces for BigFred OS daemons:

- **datadir** — `$DATA_DIR` / `$BIGFRED_DATA_DIR` → `/data`
- **config** — JSON file load + inotify hot-reload
- **ipc** — length-prefixed JSON Unix socket and a `Command` trait/router

```toml
[dependencies]
dcc-daemon = { git = "https://github.com/dcc-bigfred/rust-commons.git", branch = "main" }
```

Features: `ipc` (framing, bind, `Command` router), `config` (JSON load + inotify watch). Default enables both.

Config watches late-attach directories that do not exist at spawn (drop-in trees). A failed `inotify` watch on one path is skipped; other specs keep running.

Until the first crates.io release, use the git dependency above.

## IPC is always a singleton

A control socket has **one** live listener. `bind` / `claim` / `Server::bind` never steal a path that another process is already serving.

1. **Probe** — `connect(2)` the socket path.
2. **Live peer** — if connect succeeds, return `BindError::AlreadyRunning` (`"{name} already running at {path}"`, with pid when `SO_PEERCRED` is available). The inode is **not** unlinked. The running daemon and its clients keep the socket.
3. **Stale leftover** — if connect fails with `ENOENT` or `ECONNREFUSED` (crash left a `.sock` file), unlink that file, then bind.
4. **Bind** — only after the path is free.

There is no “unlink and take over” mode. A second instance that started while the first is healthy must fail and exit; it must not replace the listener and leave the old process with a dangling fd.

Callers map `AlreadyRunning` onto their own error type, but the wire/path layout of the daemon is unchanged. Tests should keep asserting that the display string contains `already running`.
