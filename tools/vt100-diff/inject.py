#!/usr/bin/env python3
"""inject.py VT100_DIR NAME | --list : plant one small, rare behaviour change in a
copy of vt100 so run.sh selfcheck can confirm the checker notices it.

Each mutation changes one value in every 499th matching event (a wrapped flag,
a row flag, a history time, one character), counted per process, so it is
the kind of single-byte difference a refactor could introduce.
"""
import sys
from pathlib import Path

HIT = ('{ static N: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0); '
       'N.fetch_add(1, core::sync::atomic::Ordering::Relaxed) % 499 == 498 }')

# name: (file, exact text, replacement, modes that must catch it)
MUTATIONS = {
    'wrapped': ('src/row.rs', 'self.wrapped = wrap;', f'self.wrapped = wrap && !{HIT};', ['fuzz', 'resize-fuzz']),
    'flags': ('src/row.rs', 'self.flags = flags;', f'self.flags = flags ^ u8::from(flags != 0 && {HIT});', ['fuzz', 'resize-fuzz']),
    'time': ('src/row.rs', 'self.time = time;', f'self.time = time + u64::from(time != 0 && {HIT});', ['fuzz', 'resize-fuzz']),
    'text': ('src/screen.rs', 'pub fn text(&mut self, c: char) {',
             f"pub fn text(&mut self, c: char) {{ let c = if c == 'q' && {HIT} {{ 'r' }} else {{ c }};", ['fuzz']),
}


def main():
    if sys.argv[1:] == ['--list']:
        for name, m in MUTATIONS.items():
            print(name, ','.join(m[3]))
        return 0
    if len(sys.argv) != 3 or sys.argv[2] not in MUTATIONS:
        print(__doc__.strip(), file=sys.stderr)
        return 2
    f, old, new, _ = MUTATIONS[sys.argv[2]]
    path = Path(sys.argv[1]) / f
    src = path.read_text(encoding='utf-8')
    if src.count(old) != 1:
        print(f'inject: {old!r} occurs {src.count(old)} times in {path}, expected once', file=sys.stderr)
        return 1
    path.write_text(src.replace(old, new), encoding='utf-8')
    return 0


if __name__ == '__main__':
    sys.exit(main())
