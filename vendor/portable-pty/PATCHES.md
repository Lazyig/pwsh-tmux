Based on portable-pty 0.9.0 from crates.io (MIT; see LICENSE.md). Only the
library sources are kept (no examples, no dev-dependencies).

Local addition: `CommandBuilder::raw_arg` appends an argument that the Windows
command line gets as it is, without the CommandLineToArgvW quoting the other
arguments get (`"` around it and `\"` for a quote inside), like std's
`std::os::windows::process::CommandExt::raw_arg`. cmd.exe reads its own command
line and knows no `\"` escape, so pwsh-tmux gives it `/s /c "<command>"` this way
for panes and popups. On Unix a raw argument is a plain argument.
