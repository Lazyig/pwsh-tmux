"""Deterministic pane contents for terminal UI comparison (no shell prompts)."""
import os
import sys
import time


if os.name == 'nt':
    import ctypes
    kernel = ctypes.windll.kernel32
    output = kernel.GetStdHandle(-11)
    mode = ctypes.c_ulong()
    if kernel.GetConsoleMode(output, ctypes.byref(mode)):
        kernel.SetConsoleMode(output, mode.value | 4)


def draw():
    unicode_line = ('Unicode: ascii-only control' if '--ascii' in sys.argv
                    else 'Unicode: \ud55c\uae00 \u4e2d\u6587 \u65e5\u672c\u8a9e caf\u00e9 e\u0301')
    if '--no-combining' in sys.argv:
        unicode_line = 'Unicode: \ud55c\uae00 \u4e2d\u6587 \u65e5\u672c\u8a9e caf\u00e9 e'
    if '--combining-only' in sys.argv:
        unicode_line = 'Unicode: cafe e\u0301'
    lines = [
        'PANE FIXTURE',
        'ASCII abcdefghijklmnopqrstuvwxyz 0123456789',
        unicode_line,
        '\x1b[31mRED\x1b[0m \x1b[1mBOLD\x1b[0m \x1b[4mUNDERLINE\x1b[0m \x1b[38;5;208mORANGE\x1b[0m',
        'first needle alpha',
        'second needle beta',
        'selection target: alpha beta gamma',
    ]
    if '--progress-colors' in sys.argv:
        lines += [
            'Current session',
            '\x1b[48;2;82;82;112m\x1b[38;2;180;180;255m'
            + '\u2588' + ' ' * 39 + '\x1b[0m 2% used',
        ]
    text = '\x1b[0m\x1b[2J\x1b[H\x1b]2;pane-fixture\x07'
    for row, line in enumerate(lines):
        text += '\x1b[%d;1H%s' % (row + 1, line)
    text += '\x1b[9;13H\x1b[?25h'
    sys.stdout.buffer.write(text.encode('utf-8'))
    sys.stdout.buffer.flush()


previous = None
while True:
    size = os.get_terminal_size(sys.stdout.fileno())
    if size != previous:
        draw()
        previous = size
    time.sleep(.05)
