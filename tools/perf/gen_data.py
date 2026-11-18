#!/usr/bin/env python3
"""gen_data.py [DIR] : write the benchmark inputs bench.py reads (default /work/data).

  text50.txt    50 MiB of numbered lorem-ipsum lines (4-16 words)
  colour50.txt  50 MiB of the same words with 256-colour, RGB, background and
                underline SGR sequences, each line ending in SGR 0
  text60k.txt   the first 60000 lines of text50.txt (the history tests)

The files are deterministic (random.Random(42)); existing files are kept.
colour50 continues the random stream after text50, so when only colour50 is
missing the text50 stream is replayed without writing it. The data files are
not committed; generate them where the benchmark runs.
"""
import os
import random
import sys

WORDS = ('lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod tempor incididunt ut labore '
         'et dolore magna aliqua enim ad minim veniam quis nostrud exercitation ullamco laboris nisi').split()
SIZE = 50 * 1024 * 1024


def text_lines(rnd):
    n = i = 0
    while n < SIZE:
        line = f'{i:08d} ' + ' '.join(rnd.choice(WORDS) for _ in range(rnd.randint(4, 16))) + '\n'
        yield line
        n += len(line); i += 1


def colour_lines(rnd):
    n = i = 0
    while n < SIZE:
        segs = []
        for _ in range(rnd.randint(4, 12)):
            w = rnd.choice(WORDS)
            k = rnd.randint(0, 3)
            if k == 0:
                segs.append(f'\x1b[38;5;{rnd.randint(0,255)}m{w}')
            elif k == 1:
                segs.append(f'\x1b[1;38;2;{rnd.randint(0,255)};{rnd.randint(0,255)};{rnd.randint(0,255)}m{w}')
            elif k == 2:
                segs.append(f'\x1b[48;5;{rnd.randint(0,255)};3{rnd.randint(0,7)}m{w}\x1b[49m')
            else:
                segs.append(f'\x1b[4;9{rnd.randint(0,7)}m{w}\x1b[24m')
        line = f'{i:08d} ' + ' '.join(segs) + '\x1b[0m\n'
        yield line
        n += len(line); i += 1


def write(path, lines):
    with open(path + '.tmp', 'w') as f:
        for line in lines:
            f.write(line)
    os.replace(path + '.tmp', path)


def make_data(data):
    os.makedirs(data, exist_ok=True)
    rnd = random.Random(42)
    tp, cp, hp = (os.path.join(data, f) for f in ('text50.txt', 'colour50.txt', 'text60k.txt'))
    if not os.path.exists(tp):
        write(tp, text_lines(rnd))
    elif not os.path.exists(cp):
        for _ in text_lines(rnd):
            pass
    if not os.path.exists(cp):
        write(cp, colour_lines(rnd))
    if not os.path.exists(hp):
        with open(tp) as src:
            write(hp, (src.readline() for _ in range(60000)))


if __name__ == '__main__':
    if len(sys.argv) > 2 or sys.argv[1:2] in (['-h'], ['--help']):
        print(__doc__.strip())
        sys.exit(2)
    make_data(sys.argv[1] if len(sys.argv) > 1 else '/work/data')
