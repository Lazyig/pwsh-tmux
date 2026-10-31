# Security model and validation

The server runs commands and shells as its operating-system user. Write access
to a server therefore grants command execution as that user. Only grant it to
users you trust with that account.

`server-access -r` and `attach-session -r` restrict commands and terminal input.
Read-only clients cannot clear that restriction, start shell jobs through format
strings, modify shared pane state, or operate on other clients. Their format
queries do not reveal cached shell-job output. Hooks triggered by restricted
requests are suppressed, and their terminal sizes do not resize shared windows.
This is intentionally stricter than same-user read-only control-mode behaviour
observed in tmux 3.6a.

Read-only access still exposes session contents and permitted query results,
including buffers, options and environment values. It does not isolate secrets
between sessions. Do not share a server containing information the viewer must
not read. A user who already owns the server account can open a separate writable
connection; read-only attachment does not remove their operating-system rights.

Peer identity comes from the operating system, not client-supplied fields.
Unknown identities are refused. On Windows, failure to construct an explicit
owner ACL prevents the listener from starting. The first frame must identify the
client, and authorization runs after its header, before its body is allocated.

Resource limits include 64 simultaneous connections, 1 MiB client frames, a
64-event input queue, 8 MiB queued output per client, a 10-second identify deadline
and a 10-second output-write deadline. Output overflow disconnects the client.
Command replies have a 4 MiB aggregate limit, including multi-command requests.
Bracketed paste is capped at 4 MiB; oversized pastes are discarded completely
through their end marker. Control subscriptions are limited to 128 per client,
with formats of at most 64 KiB.
Subscription configuration and cached values each have a 1 MiB aggregate limit
per client; cache overflow disconnects that client. Format output, expansion work, padding, numeric
precision, regex replacement and glob matching also have limits. These limits
reduce resource exhaustion; they are not a guarantee of availability against all
workloads.

Regression tests cover ACL read-only commands, lost input flags, alternate format
targets, shell-job cache access, hooks, actual control clients and attached
terminals, invalid frame headers, stalled I/O, output overflow and adversarial
formats. Run them with `cargo test --locked`. SSH tests require a verified
`known_hosts` entry and explicit `PT_SSH` / `PT_SSH_BIN` settings.

Windows tests do not establish Linux or real cross-user coverage. Run
`python3 tests/security_multiuser.py /absolute/path/to/pwsh-tmux` as Unix root to
test real separate UIDs without creating accounts or installing executables.
Current platform results are recorded in `VALIDATION.md`.
Passing tests cannot establish that all vulnerabilities are absent. Please
report suspected vulnerabilities privately to the maintainer before publishing
exploit details.
