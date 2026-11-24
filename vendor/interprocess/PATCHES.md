Based on interprocess 2.4.7 from crates.io (0BSD OR Apache-2.0; see
LICENSE-0BSD.txt and LICENSE-APACHE.txt). The library sources are kept with
README.md (lib.rs includes it as the crate documentation) and examples/ (doc
builds include them); the inspect-platform binary, the tests and the
dev-dependencies are not. The crate replaces the registry copy through
`[patch.crates-io]` in the project Cargo.toml.

Cargo.toml: besides those removals, the `[lints.rust]` entry
`semicolon_in_expressions_from_non_local_macros = "allow"` is gone. Current Rust
no longer knows that lint and warns about it; lints of a registry crate are
capped, but those of a bundled one are not.

Local change: src/os/windows/named_pipe/local_socket/listener.rs creates the
named pipe of a local socket listener with 64 KiB input and output buffers
(`LOCAL_SOCKET_BUFFER_SIZE`). Upstream keeps the `PipeListenerOptions` default
of 512 bytes for both. Only local socket listeners change; `PipeListenerOptions`
itself and its default stay as upstream.

Why: the pwsh-tmux server puts each client pipe in nonblocking mode
(`PIPE_NOWAIT`). A nonblocking WriteFile takes only what fits in the pipe buffer
(or what a pending client read can take at once) and the server then waits 5 ms
before trying again (src/server/transport.rs). With 512-byte buffers that held
output to about 100 KB/s per client in theory and about 55 KB/s as measured.

Why 64 KiB:
- One write takes a full redraw of a usual terminal. A 120x32 screen with a
  256-colour change in every cell (`ESC[38;5;Nm` and the character) is about
  46 KB.
- Even if every write ended in the 5 ms wait, the ceiling would be about
  13 MB/s per client, well above what a terminal draws.
- It is the default capacity of a Linux pipe and four times the 16 KiB a Linux
  send of the server takes per call.
- The buffer counts against nonpaged pool. With at most 64 clients
  (`MAX_CLIENTS`) and two directions it is at most 8 MiB in all, the same as
  the output backlog one client may have queued in the server. Windows treats
  the size as a hint and rounds or clamps it.
The larger input buffer also lets a big paste reach the server in fewer writes.

Review this change when updating interprocess.
