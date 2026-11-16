#!/usr/bin/env python3
"""Extract tmux 3.8 prefix/root defaults, expanding C string macros.
Usage: extract-display-defaults.py /path/to/tmux-3.8/key-bindings.c
Floating pane bindings are maintained by their work package; the switch-mode
bindings (Tab, BTab), which open a floating pane, are kept.
"""
import ast
import re
import sys
from pathlib import Path
src = Path(sys.argv[1]).read_text()
macros = {}
pattern = r'"(?:\\.|[^"\\])*"|DEFAULT_[A-Z_]+'
def expand(s):
    return ''.join(ast.literal_eval(t) if t.startswith('"') else expand(macros[t])
                   for t in re.findall(pattern, s))
lines = iter(src.splitlines())
for line in lines:
    m = re.match(r'#define (DEFAULT_\w+) (.*)', line)
    if not m:
        continue
    value = m[2]
    while value.endswith('\\'):
        value = value[:-1] + next(lines)
    macros[m[1]] = value
body = src.split('static const char *const defaults[] = {', 1)[1].split('/* Copy mode (emacs)', 1)[0]
excluded = ('new-pane', 'move-pane', 'break-pane -W', 'floating_pane_flag', 'MouseDown1Control7')
print('# Generated from tmux 3.8 key-bindings.c by tools/extract-display-defaults.py.')
print('# Floating pane bindings are owned by another package.')
for line in body.splitlines():
    if not line.lstrip().startswith('"'):
        continue
    cmd = expand(line)
    if '-Tmove' in cmd or any(x in cmd for x in excluded) and 'DEFAULT_' not in line and 'switch-mode' not in cmd:
        continue
    print(cmd)
