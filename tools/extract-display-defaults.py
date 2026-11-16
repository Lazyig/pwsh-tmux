#!/usr/bin/env python3
"""Extract tmux 3.8 prefix/root/move defaults, expanding C string macros.
Usage: extract-display-defaults.py /path/to/tmux-3.8/key-bindings.c
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
print('# Generated from tmux 3.8 key-bindings.c by tools/extract-display-defaults.py.')
for line in body.splitlines():
    if not line.lstrip().startswith('"'):
        continue
    cmd = expand(line)
    print(cmd)
