#!/usr/bin/env python3
"""Render latency of an attached client under continuous pane output.

usage: render_latency.py OUT.json TESTS IMPL=BIN [IMPL=BIN ...]
  TESTS: comma list of cat,seq,flood,steady,keys (or all)
Env: REPS (default 5), WORK (scratch directory, default a new temporary one; the 50 MB
  text file is made in WORK/data), READ_RATE (bytes/s the client's terminal reads, 0 =
  as fast as it can), STEADY_SECS, FLOOD_SECS, KEYS_N.

cat/seq: the pane runs `cat FILE` / `seq 1 2000000`, then prints ENDMARK and records the
  time (date +%s%N, CLOCK_REALTIME) right after the marker's write returned. Latency is
  the time the attached client's pty received the marker minus that timestamp. Also the
  total time, the server CPU (utime+stime) and the client's bytes.
flood: the pane writes 'S<seq> T<ns>' lines as fast as it can for FLOOD_SECS: how stale
  the lines the client is shown are, the longest gap between updates, and the latency of
  the last line.
steady: the pane writes 'S<seq> T<ns>' 100 times a second for STEADY_SECS; latency of a
  line is its first arrival at the client minus its T (time.time_ns() just after write).
keys: key echo latency in a `stty -icanon -echo; exec cat` pane while a neighbouring
  pane runs `while :; do seq 1 100000; done`.
Every server uses a private -L socket and -f /dev/null; servers are killed by pid.
Implementations alternate order per repetition.
"""
import ctypes, fcntl, json, os, pty, random, re, signal, struct, subprocess, sys, termios, threading, time

import tempfile
HERE = os.environ.get('WORK') or tempfile.mkdtemp(prefix='render-latency-')
REPS = int(os.environ.get('REPS', '5'))
DATA = f'{HERE}/data'
STEADY_SECS = float(os.environ.get('STEADY_SECS', '5'))
FLOOD_SECS = float(os.environ.get('FLOOD_SECS', '3'))
KEYS_N = int(os.environ.get('KEYS_N', '200'))
# bytes/s the client's terminal reads (0: as fast as it can): a slow terminal emulator
READ_RATE = float(os.environ.get('READ_RATE', '0'))
CLK = os.sysconf('SC_CLK_TCK')
ESC = re.compile(rb'\x1b\[[0-?]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1bP[^\x1b]*\x1b\\|\x1b[()*+][0-~]|\x1b[ -/]*[0-~]')
KEYS = b'jkwyz'
ENV = dict(os.environ, TERM='xterm-256color', SHELL='/bin/sh', LANG='C.UTF-8')
for k in ('TMUX', 'PWSH_TMUX', 'PWSH_TMUX_PANE'):
    ENV.pop(k, None)
os.environ.clear(); os.environ.update(ENV)
try:
    ctypes.CDLL(None).prctl(36, 1, 0, 0, 0)  # PR_SET_CHILD_SUBREAPER
except Exception:
    pass


def reap():
    while True:
        try:
            pid, _ = os.waitpid(-1, os.WNOHANG)
        except ChildProcessError:
            return
        if pid == 0:
            return


def log(*a):
    print(time.strftime('%H:%M:%S'), *a, file=sys.stderr, flush=True)


def pcpu(pid):
    try:
        with open(f'/proc/{pid}/stat') as f:
            s = f.read()
    except OSError:
        return None
    r = s[s.rfind(')') + 2:].split()
    return {'state': r[0], 'cpu': (int(r[11]) + int(r[12])) / CLK}


class Impl:
    def __init__(self, name, binary):
        self.name, self.bin = name, binary
        self.sock = None
        self.pid = None

    def c(self, *args, check=True, timeout=60):
        r = subprocess.run([self.bin, '-L', self.sock, *args], capture_output=True, text=True, timeout=timeout)
        if check and r.returncode != 0:
            raise RuntimeError(f'{self.name} {args}: rc={r.returncode} {r.stderr.strip()}')
        return r.stdout.rstrip('\n')

    def start(self, tag, *newargs):
        self.sock = f'rl-{self.name}-{tag}-{os.getpid()}'
        r = subprocess.run([self.bin, '-L', self.sock, '-f', '/dev/null', 'new', '-d', *newargs], capture_output=True, text=True, timeout=60)
        if r.returncode != 0:
            raise RuntimeError(f'{self.name} start: {r.stderr}')
        self.pid = int(self.c('display', '-p', '#{pid}'))

    def kill(self):
        if not self.sock:
            return
        subprocess.run([self.bin, '-L', self.sock, 'kill-server'], capture_output=True, timeout=30)
        if self.pid:
            end = time.time() + 10
            while time.time() < end:
                s = pcpu(self.pid)
                if s is None or s['state'] in 'ZX':
                    break
                time.sleep(0.02)
            else:
                try:
                    os.kill(self.pid, signal.SIGKILL)
                except OSError:
                    pass
        self.pid = None
        time.sleep(0.1)
        reap()

    def cpu(self):
        return pcpu(self.pid)['cpu']


class Client:
    """An attached client on a pty drained by a thread that timestamps what it reads.

    watch(regex): records (time_ns of the read, match) for every new match in the
    escape-stripped stream; a match is looked for in each read joined with the tail of
    the one before, so one split across reads is found too."""

    def __init__(self, impl, target, cols=120, rows=40):
        m, s = pty.openpty()
        fcntl.ioctl(s, termios.TIOCSWINSZ, struct.pack('HHHH', rows, cols, 0, 0))

        def pre():
            os.setsid()
            fcntl.ioctl(0, termios.TIOCSCTTY, 0)
        self.p = subprocess.Popen([impl.bin, '-L', impl.sock, 'attach', '-t', target], stdin=s, stdout=s, stderr=s,
                                  preexec_fn=pre, close_fds=True)
        os.close(s)
        self.m = m
        self.cv = threading.Condition()
        self.total = 0
        self.tail = b''
        self.watches = []   # (regex, key-fn, dict first-seen key -> ns)
        self.th = threading.Thread(target=self._rd, daemon=True)
        self.th.start()

    def watch(self, rx, key=lambda m: m.group(0)):
        d = {}
        with self.cv:
            self.watches.append((re.compile(rx), key, d))
        return d

    def unwatch_all(self):
        with self.cv:
            self.watches = []

    def _rd(self):
        t_next = time.monotonic()
        while True:
            if READ_RATE:
                left = t_next - time.monotonic()
                if left > 0:
                    time.sleep(left)
            try:
                d = os.read(self.m, 4096 if READ_RATE else 1 << 20)
                if READ_RATE:
                    t_next = max(t_next, time.monotonic() - 0.01) + len(d) / READ_RATE
            except OSError:
                break
            if not d:
                break
            now = time.time_ns()
            with self.cv:
                self.total += len(d)
                if self.watches:
                    seg = self.tail + d
                    text = ESC.sub(b'', seg)
                    for rx, key, seen in self.watches:
                        for mt in rx.finditer(text):
                            k = key(mt)
                            if k not in seen:
                                seen[k] = now
                    self.tail = seg[-512:]
                self.cv.notify_all()
        with self.cv:
            self.cv.notify_all()

    def wait_key(self, d, k, timeout):
        end = time.time() + timeout
        with self.cv:
            while k not in d:
                left = end - time.time()
                if left <= 0 or self.p.poll() is not None:
                    raise TimeoutError(f'{k!r} not seen')
                self.cv.wait(min(left, 0.2))
            return d[k]

    def write(self, b):
        os.write(self.m, b)

    def close(self):
        try:
            self.p.send_signal(signal.SIGHUP)
            self.p.wait(5)
        except Exception:
            self.p.kill()
            self.p.wait()
        try:
            os.close(self.m)
        except OSError:
            pass


def make_data():
    os.makedirs(DATA, exist_ok=True)
    words = ('lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod tempor incididunt ut labore '
             'et dolore magna aliqua enim ad minim veniam quis nostrud exercitation ullamco laboris nisi').split()
    rnd = random.Random(42)
    tp = f'{DATA}/text50.txt'
    if not os.path.exists(tp):
        with open(tp + '.tmp', 'w') as f:
            n = 0; i = 0
            while n < 50 * 1024 * 1024:
                line = f'{i:08d} ' + ' '.join(rnd.choice(words) for _ in range(rnd.randint(4, 16))) + '\n'
                f.write(line); n += len(line); i += 1
        os.rename(tp + '.tmp', tp)
    return tp


def t_bulk(impl, what, tag):
    """cat of a 50 MB text file or seq 1 2000000: marker latency, total time, server CPU."""
    src = f'cat {DATA}/text50.txt' if what == 'cat' else 'seq 1 2000000'
    ts = f'{HERE}/ts-{os.getpid()}'
    os.makedirs(os.path.dirname(ts), exist_ok=True)
    try:
        os.unlink(ts)
    except OSError:
        pass
    impl.start(tag, '-s', 's', '-x', '120', '-y', '39', 'exec cat')
    try:
        cl = Client(impl, 's')
        st = cl.watch(rb'\[s\]')
        cl.wait_key(st, b'[s]', 10)
        time.sleep(0.3)
        mk = cl.watch(rb'ENDMARK')
        c0 = impl.cpu(); t0 = time.time_ns(); b0 = cl.total
        impl.c('new-window', '-t', 's:', '-n', 'tp', f"{src}; printf 'END''MARK\\n'; date +%s%N > {ts}; exec cat")
        t1 = cl.wait_key(mk, b'ENDMARK', 300)
        # the server is still busy until the client has the last frame: let it settle
        time.sleep(0.3)
        c1 = impl.cpu()
        for _ in range(200):
            if os.path.exists(ts) and os.path.getsize(ts) > 0:
                break
            time.sleep(0.01)
        with open(ts) as f:
            tw = int(f.read().strip())
        cl.close()
        return {'lat_ms': round((t1 - tw) / 1e6, 2), 'total_s': round((t1 - t0) / 1e9, 3),
                'server_cpu_s': round(c1 - c0, 2), 'client_bytes': cl.total - b0}
    finally:
        impl.kill()


STEADY = ("import sys,time,os\n"
          "n=int(sys.argv[1]); w=os.write; t=time.monotonic()\n"
          "for i in range(n):\n"
          "    w(1, b'S%07d T' % i); w(1, b'%d\\n' % time.time_ns())\n"
          "    t += 0.01\n"
          "    d = t - time.monotonic()\n"
          "    if d > 0: time.sleep(d)\n"
          "w(1, b'STEADYDONE\\n')\n")


def t_steady(impl, tag):
    """100 lines/s: per-line latency from the write (T in the line) to first arrival."""
    script = f'{HERE}/steady.py'
    with open(script, 'w') as f:
        f.write(STEADY)
    n = int(STEADY_SECS * 100)
    impl.start(tag, '-s', 's', '-x', '120', '-y', '39', 'exec cat')
    try:
        cl = Client(impl, 's')
        st = cl.watch(rb'\[s\]')
        cl.wait_key(st, b'[s]', 10)
        time.sleep(0.3)
        seen = cl.watch(rb'S(\d{7}) T(\d{19})', key=lambda m: (int(m.group(1)), int(m.group(2))))
        done = cl.watch(rb'STEADYDONE')
        c0 = impl.cpu(); t0 = time.time()
        impl.c('new-window', '-t', 's:', '-n', 'st', f'python3 -u {script} {n}; exec cat')
        cl.wait_key(done, b'STEADYDONE', STEADY_SECS + 30)
        c1 = impl.cpu(); t1 = time.time()
        with cl.cv:
            items = dict(seen)
        firsts = {}
        for (i, tns), arr in items.items():
            if i not in firsts or arr < firsts[i]:
                firsts[i] = arr - tns
        lats = sorted(v / 1e6 for v in firsts.values())
        cl.close()
        return {'lines_seen': len(lats), 'lines': n, 'p50_ms': pct(lats, 50), 'p90_ms': pct(lats, 90), 'p99_ms': pct(lats, 99),
                'max_ms': round(lats[-1], 2), 'server_cpu_pct': round(100 * (c1 - c0) / (t1 - t0), 1)}
    finally:
        impl.kill()


FLOOD = ("import sys,time,os\n"
         "secs=float(sys.argv[1]); w=os.write; end=time.monotonic()+secs; i=0\n"
         "while time.monotonic() < end:\n"
         "    b = b''.join(b'S%07d T%d\\n' % (i + k, time.time_ns()) for k in range(8))\n"
         "    w(1, b); i += 8\n"
         "w(1, b'FLOODDONE T%d\\n' % time.time_ns())\n")


def t_flood(impl, tag):
    """As fast as the pane can write, lines carrying their write time: how stale what the
    client is shown is while the flood goes on, the longest gap between updates, and the
    latency of the last line."""
    script = f'{HERE}/flood.py'
    with open(script, 'w') as f:
        f.write(FLOOD)
    impl.start(tag, '-s', 's', '-x', '120', '-y', '39', 'exec cat')
    try:
        cl = Client(impl, 's')
        st = cl.watch(rb'\[s\]')
        cl.wait_key(st, b'[s]', 10)
        time.sleep(0.3)
        seen = cl.watch(rb'S(\d{7}) T(\d{19})', key=lambda m: (int(m.group(1)), int(m.group(2))))
        done = cl.watch(rb'FLOODDONE T(\d{19})', key=lambda m: int(m.group(1)))
        c0 = impl.cpu(); t0 = time.time()
        impl.c('new-window', '-t', 's:', '-n', 'fl', f'python3 -u {script} {FLOOD_SECS}; exec cat')
        end = time.time() + FLOOD_SECS + 60
        while not done and time.time() < end:
            time.sleep(0.05)
        c1 = impl.cpu(); t1 = time.time()
        disc = impl.c('list-clients', '-F', '#{client_discarded} #{client_written}', check=False)
        with cl.cv:
            items = dict(seen); fin = dict(done)
        if not fin:
            raise TimeoutError('FLOODDONE not seen')
        (tw, ta), = fin.items()
        firsts = {}
        for (i, tns), arr in items.items():
            if i not in firsts or arr < firsts[i][0]:
                firsts[i] = (arr, tns)
        arrs = sorted(set(a for a, _ in firsts.values()))
        lats = sorted((a - t) / 1e6 for a, t in firsts.values())
        gaps = [(b - a) / 1e6 for a, b in zip(arrs, arrs[1:])]
        cl.close()
        return {'end_lat_ms': round((ta - tw) / 1e6, 2), 'shown_p50_ms': pct(lats, 50), 'shown_p90_ms': pct(lats, 90),
                'shown_max_ms': round(lats[-1], 2), 'max_gap_ms': round(max(gaps), 2) if gaps else None,
                'updates': len(arrs), 'lines_written': max(firsts) if firsts else 0, 'discarded_written': disc,
                'server_cpu_pct': round(100 * (c1 - c0) / (t1 - t0), 1)}
    finally:
        impl.kill()


def t_keys(impl, tag):
    impl.start(tag, '-s', 'L', '-x', '120', '-y', '39', 'stty -icanon -echo; exec cat')
    try:
        impl.c('split-window', '-d', '-t', 'L:0', 'while :; do seq 1 100000; done')
        cl = Client(impl, 'L')
        st = cl.watch(rb'\[L\]')
        cl.wait_key(st, b'[L]', 10)
        time.sleep(1)
        lats = []
        c0 = impl.cpu(); t0 = time.time()
        for i in range(KEYS_N):
            k = KEYS[i % len(KEYS):i % len(KEYS) + 1]
            d = cl.watch(re.escape(k))
            ts = time.time_ns()
            cl.write(k)
            ta = cl.wait_key(d, k, 5)
            cl.unwatch_all()
            lats.append((ta - ts) / 1e6)
            time.sleep(random.uniform(0.005, 0.015))
        c1 = impl.cpu(); t1 = time.time()
        lats.sort()
        cl.close()
        return {'p50_ms': pct(lats, 50), 'p90_ms': pct(lats, 90), 'p99_ms': pct(lats, 99), 'max_ms': round(lats[-1], 2),
                'server_cpu_pct': round(100 * (c1 - c0) / (t1 - t0), 1)}
    finally:
        impl.kill()


def pct(v, p):
    s = sorted(v)
    return round(s[min(len(s) - 1, int(round(p / 100 * (len(s) - 1))))], 2)


def main():
    out_path, tests = sys.argv[1], sys.argv[2]
    impls = [Impl(*a.split('=', 1)) for a in sys.argv[3:]]
    tests = ['cat', 'seq', 'flood', 'steady', 'keys'] if tests == 'all' else tests.split(',')
    if 'cat' in tests:
        make_data()
    res = {'reps': REPS, 'impls': {i.name: i.bin for i in impls}, 'results': {}}
    for test in tests:
        for rep in range(REPS):
            order = impls if rep % 2 == 0 else impls[::-1]
            for impl in order:
                if test in ('cat', 'seq'):
                    r = t_bulk(impl, test, test)
                elif test == 'flood':
                    r = t_flood(impl, test)
                elif test == 'steady':
                    r = t_steady(impl, test)
                else:
                    r = t_keys(impl, test)
                log(test, rep, impl.name, json.dumps(r))
                res['results'].setdefault(test, {}).setdefault(impl.name, []).append(r)
                with open(out_path, 'w') as f:
                    json.dump(res, f, indent=1)
    # summary: median and range of each numeric field
    for test, by in res['results'].items():
        for name, runs in by.items():
            parts = []
            for k in runs[0]:
                v = sorted((r[k] for r in runs if r[k] is not None), key=lambda x: (isinstance(x, str), x if not isinstance(x, str) else 0, str(x))) or [None]
                parts.append(f'{k}={v[len(v) // 2]} [{v[0]}..{v[-1]}]')
            print(f'{test:7s} {name:8s} ' + '  '.join(parts))


if __name__ == '__main__':
    main()
