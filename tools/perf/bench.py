#!/usr/bin/env python3
"""Fair tmux vs pwsh-tmux benchmark. Runs inside the tmux38-perf pod.

usage: bench.py OUT.json GROUPS IMPL=BIN [IMPL=BIN ...]
  GROUPS: comma list of idle,memory,throughput,latency,commands,heavy (or all)
  e.g.   bench.py /work/res/r1.json all tmux=/work/root/usr/bin/tmux pwsh=/work/bin/pwsh-tmux-musl
Every server uses a private -L socket (b-IMPL-TEST, so give implementations
distinct names) and -f /dev/null. Repetitions alternate the implementation
order. Raw samples are written to OUT.json (rewritten after each test).
Environment: REPS (3), IDLE_SECS (30), BENCH_DATA (/work/data: inputs written
there by gen_data.py on first use).
"""
import ctypes, fcntl, json, os, pty, random, re, select, signal, struct, subprocess, sys, termios, threading, time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import gen_data  # noqa: E402

REPS = int(os.environ.get('REPS', '3'))
IDLE_SECS = float(os.environ.get('IDLE_SECS', '30'))
DATA = os.environ.get('BENCH_DATA', '/work/data')
CLK = os.sysconf('SC_CLK_TCK')
ESC = re.compile(rb'\x1b\[[0-?]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1bP[^\x1b]*\x1b\\|\x1b[()*+][0-~]|\x1b[ -/]*[0-~]')
KEYS = b'jkwyz'  # letters never present in the status line / flood output / CSI finals used here
ENV = dict(os.environ, TERM='xterm-256color', SHELL='/bin/sh', LANG='C.UTF-8')
ENV.pop('TMUX', None)
os.environ.clear(); os.environ.update(ENV)

# become a subreaper so orphaned panes of killed servers do not pile up as zombies under PID 1
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


# ---------------------------------------------------------------- /proc helpers
def pstat(pid):
    try:
        with open(f'/proc/{pid}/stat') as f:
            s = f.read()
    except OSError:
        return None
    r = s[s.rfind(')') + 2:].split()
    d = {'state': r[0], 'cpu': (int(r[11]) + int(r[12])) / CLK, 'utime': int(r[11]) / CLK,
         'stime': int(r[12]) / CLK, 'threads': int(r[17])}
    vol = nvol = 0
    try:
        for t in os.listdir(f'/proc/{pid}/task'):
            with open(f'/proc/{pid}/task/{t}/status') as f:
                for line in f:
                    if line.startswith('voluntary_ctxt_switches'):
                        vol += int(line.split()[1])
                    elif line.startswith('nonvoluntary_ctxt_switches'):
                        nvol += int(line.split()[1])
    except OSError:
        pass
    d['vcsw'] = vol; d['nvcsw'] = nvol
    try:
        with open(f'/proc/{pid}/status') as f:
            for line in f:
                k = line.split(':')[0]
                if k in ('VmRSS', 'RssAnon', 'RssFile', 'VmHWM'):
                    d[k] = int(line.split()[1])  # kB
        with open(f'/proc/{pid}/smaps_rollup') as f:
            for line in f:
                k = line.split(':')[0]
                if k in ('Pss', 'Private_Dirty', 'Private_Clean'):
                    d[k] = int(line.split()[1])
    except OSError:
        pass
    return d


def cg_throttle():
    try:
        with open('/sys/fs/cgroup/cpu.stat') as f:
            d = dict(l.split() for l in f)
        return int(d.get('nr_throttled', 0)), int(d.get('throttled_usec', 0))
    except OSError:
        return 0, 0


def delta(a, b, keys=('cpu', 'utime', 'stime', 'vcsw', 'nvcsw')):
    return {k: round(b[k] - a[k], 4) for k in keys}


def thread_names(pid):
    out = {}
    try:
        for t in os.listdir(f'/proc/{pid}/task'):
            with open(f'/proc/{pid}/task/{t}/comm') as f:
                n = f.read().strip()
            out[n] = out.get(n, 0) + 1
    except OSError:
        pass
    return out


def children(pid):
    out = []
    for p in os.listdir('/proc'):
        if not p.isdigit():
            continue
        try:
            with open(f'/proc/{p}/stat') as f:
                s = f.read()
            r = s[s.rfind(')') + 2:].split()
            if int(r[1]) == pid:
                out.append((int(p), s[s.find('(') + 1:s.rfind(')')]))
        except OSError:
            pass
    return out


# ---------------------------------------------------------------- implementation wrapper
class Impl:
    def __init__(self, name, binary):
        self.name, self.bin = name, binary
        self.sock = None

    def c(self, *args, check=True, timeout=120):
        r = subprocess.run([self.bin, '-L', self.sock, *args], capture_output=True, text=True, timeout=timeout)
        if check and r.returncode != 0:
            raise RuntimeError(f'{self.name} {args}: rc={r.returncode} {r.stderr.strip()}')
        return r.stdout.rstrip('\n')

    def popen(self, *args):
        return subprocess.Popen([self.bin, '-L', self.sock, *args], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

    def start(self, tag, *newargs):
        self.sock = f'b-{self.name}-{tag}'
        self.kill()
        t0 = time.perf_counter()
        r = subprocess.run([self.bin, '-L', self.sock, '-f', '/dev/null', 'new', '-d', *newargs], capture_output=True, text=True, timeout=60)
        t1 = time.perf_counter()
        if r.returncode != 0:
            raise RuntimeError(f'{self.name} start: {r.stderr}')
        self.pid = int(self.c('display', '-p', '#{pid}'))
        return t1 - t0

    def kill(self):
        if not self.sock:
            return
        pid = getattr(self, 'pid', None)
        subprocess.run([self.bin, '-L', self.sock, 'kill-server'], capture_output=True, timeout=30)
        if pid:
            end = time.time() + 10
            while time.time() < end:
                s = pstat(pid)
                if s is None or s['state'] in 'ZX':
                    break
                time.sleep(0.02)
            else:
                try:
                    os.kill(pid, signal.SIGKILL)
                except OSError:
                    pass
        self.pid = None
        time.sleep(0.2)
        reap()

    def snap(self):
        return pstat(self.pid)

    def wait_capture(self, target, text, timeout=300, interval=0.005):
        end = time.time() + timeout
        while time.time() < end:
            if text in self.c('capture-pane', '-p', '-t', target):
                return time.perf_counter()
            time.sleep(interval)
        raise TimeoutError(f'{self.name}: {text} not shown in {target}')


class Client:
    """A tmux client attached through a pty we drain in a thread."""

    def __init__(self, impl, target, cols=120, rows=40):
        m, s = pty.openpty()
        fcntl.ioctl(s, termios.TIOCSWINSZ, struct.pack('HHHH', rows, cols, 0, 0))

        def pre():
            os.setsid()
            fcntl.ioctl(0, termios.TIOCSCTTY, 0)
        self.t_spawn = time.perf_counter()
        self.p = subprocess.Popen([impl.bin, '-L', impl.sock, 'attach', '-t', target], stdin=s, stdout=s, stderr=s,
                                  preexec_fn=pre, close_fds=True)
        os.close(s)
        self.m = m
        self.cv = threading.Condition()
        self.total = 0
        self.buf = bytearray()   # output since mark(), trimmed from the front past 1 MiB
        self.base = 0            # absolute stream offset of buf[0]
        self.scan = 0            # absolute offset already searched
        self.last_t = 0.0
        self.th = threading.Thread(target=self._rd, daemon=True)
        self.th.start()

    def _rd(self):
        while True:
            try:
                d = os.read(self.m, 262144)
            except OSError:
                break
            if not d:
                break
            now = time.perf_counter()
            with self.cv:
                self.total += len(d)
                self.buf += d
                if len(self.buf) > 1048576:
                    drop = len(self.buf) - 262144
                    del self.buf[:drop]
                    self.base += drop
                self.last_t = now
                self.cv.notify_all()
        with self.cv:
            self.cv.notify_all()

    def mark(self):
        with self.cv:
            self.base += len(self.buf)
            self.buf.clear()
            self.scan = self.base

    def wait(self, pat, timeout=300):
        """wait until regex pat appears in escape-stripped output since mark(); return arrival time of the chunk"""
        rx = re.compile(pat)
        end = time.perf_counter() + timeout
        with self.cv:
            while True:
                start = max(self.scan - 64, self.base)
                seg = bytes(self.buf[start - self.base:])
                self.scan = self.base + len(self.buf)
                if rx.search(ESC.sub(b'', seg)):
                    return self.last_t
                left = end - time.perf_counter()
                if left <= 0 or self.p.poll() is not None:
                    raise TimeoutError(f'pattern {pat!r} not seen; tail={ESC.sub(b"", seg)[-200:]!r}')
                self.cv.wait(min(left, 0.5))

    def write(self, b):
        os.write(self.m, b)

    def snap(self):
        return pstat(self.p.pid)

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


def mem_view(s):
    return {k: s.get(k) for k in ('VmRSS', 'RssAnon', 'RssFile', 'Pss', 'Private_Dirty', 'VmHWM', 'threads')}


# ---------------------------------------------------------------- data
def make_data():
    gen_data.make_data(DATA)


# ---------------------------------------------------------------- tests
def idle_scenario(impl, kind):
    cl = None
    if kind == 'a':
        impl.start('idle-a', '-s', 's', '-x', '120', '-y', '40')
    elif kind == 'b':
        impl.start('idle-b', '-s', 's0', '-x', '120', '-y', '40')
        for i in range(10):
            if i:
                impl.c('new', '-d', '-s', f's{i}', '-x', '120', '-y', '40')
            for w in range(4):
                if w:
                    impl.c('new-window', '-d', '-t', f's{i}:')
                impl.c('split-window', '-d', '-t', f's{i}:{w}')
        n = len(impl.c('list-panes', '-a').splitlines())
        assert n == 80, n
    else:
        impl.start('idle-c', '-s', 's', '-x', '120', '-y', '39')
        impl.c('set', '-g', 'status-interval', '1')
        impl.c('split-window', '-h', '-t', 's:0', 'seq 1 3000; exec cat')
        time.sleep(0.5)
        cl = Client(impl, 's')
        cl.wait(rb'\[s\]', 10)
        impl.c('clock-mode', '-t', 's:0.0')
        impl.c('copy-mode', '-t', 's:0.1')
    time.sleep(3)
    a = impl.snap(); ca = cl.snap() if cl else None; t0 = time.perf_counter()
    time.sleep(IDLE_SECS)
    b = impl.snap(); cb = cl.snap() if cl else None; t1 = time.perf_counter()
    res = {'secs': round(t1 - t0, 3), 'server': delta(a, b), 'mem': mem_view(b),
           'children': len(children(impl.pid)), 'server_threads': b['threads'], 'thread_names': thread_names(impl.pid)}
    res['server']['wakeups_per_s'] = round((res['server']['vcsw'] + res['server']['nvcsw']) / (t1 - t0), 3)
    if cl:
        res['client'] = delta(ca, cb); res['client_mem'] = mem_view(cb)
        res['client']['wakeups_per_s'] = round((res['client']['vcsw'] + res['client']['nvcsw']) / (t1 - t0), 3)
        res['client_bytes'] = cl.total
        cl.close()
    impl.kill()
    return res


def t_memory(impl):
    out = {}
    for name, cmd in (('hist_seq', 'seq 1 200000'), ('hist_text', f'cat {DATA}/text60k.txt')):
        impl.start('mem', '-s', 's', '-x', '120', '-y', '40', 'exec cat')
        impl.c('set', '-g', 'history-limit', '50000')
        time.sleep(1)
        before = impl.snap()
        t0 = time.perf_counter()
        impl.c('new-window', '-t', 's:', '-n', 'h', f'{cmd}; echo END""MARK; exec cat')
        t1 = impl.wait_capture('s:h', 'ENDMARK', interval=0.05)
        time.sleep(1.5)
        after = impl.snap()
        hs = impl.c('display', '-p', '-t', 's:h', '#{history_size}')
        out[name] = {'before': mem_view(before), 'after': mem_view(after), 'history_size': int(hs),
                     'delta_rss_kb': after['VmRSS'] - before['VmRSS'], 'delta_anon_kb': after['RssAnon'] - before['RssAnon'],
                     'fill_secs': round(t1 - t0, 3)}
        impl.kill()
    impl.start('mem', '-s', 's', '-x', '120', '-y', '40', 'exec cat')
    time.sleep(1)
    one = impl.snap()
    for i in range(99):
        impl.c('new-window', '-d', '-t', 's:', 'exec cat')
    assert len(impl.c('list-panes', '-a').splitlines()) == 100
    time.sleep(2)
    hund = impl.snap()
    out['panes100'] = {'one': mem_view(one), 'hundred': mem_view(hund),
                       'per_pane_rss_kb': round((hund['VmRSS'] - one['VmRSS']) / 99, 2),
                       'per_pane_anon_kb': round((hund['RssAnon'] - one['RssAnon']) / 99, 2),
                       'per_pane_pss_kb': round((hund.get('Pss', 0) - one.get('Pss', 0)) / 99, 2)}
    impl.kill()
    return out


def t_throughput(impl):
    out = {}
    for fname in ('text50', 'colour50'):
        path = f'{DATA}/{fname}.txt'
        for mode in ('noclient', 'client'):
            impl.start('tp', '-s', 's', '-x', '120', '-y', '39', 'exec cat')
            cl = None
            if mode == 'client':
                cl = Client(impl, 's')
                cl.wait(rb'\[s\]', 10)
                time.sleep(0.3)
                cl.mark()
            w = impl.popen('wait-for', 'tpdone')
            time.sleep(0.1)
            thr0 = cg_throttle()
            a = impl.snap(); ca = cl.snap() if cl else None
            t0 = time.perf_counter()
            impl.c('new-window', '-t', 's:', '-n', 'tp', f'cat {path}; {impl.bin} -L {impl.sock} wait-for -S tpdone; echo END""MARK; exec cat')
            w.wait(600)
            t_wf = time.perf_counter()
            if cl:
                t1 = cl.wait(rb'ENDMARK', 120)
            else:
                t1 = impl.wait_capture('s:tp', 'ENDMARK')
            b = impl.snap(); cb = cl.snap() if cl else None
            thr1 = cg_throttle()
            r = {'secs': round(t1 - t0, 4), 'secs_to_cat_done': round(t_wf - t0, 4), 'server': delta(a, b),
                 'mb_per_s': round(os.path.getsize(path) / 1048576 / (t1 - t0), 2), 'mem_after': mem_view(b),
                 'throttled_periods': thr1[0] - thr0[0], 'throttled_ms': (thr1[1] - thr0[1]) / 1000}
            if cl:
                r['client'] = delta(ca, cb); r['client_bytes'] = cl.total
                cl.close()
            out[f'{fname}_{mode}'] = r
            impl.kill()
    return out


def key_latency(impl, cl, n=500):
    lats = []
    for i in range(n):
        k = KEYS[i % len(KEYS):i % len(KEYS) + 1]
        cl.mark()
        t0 = time.perf_counter()
        cl.write(k)
        t1 = cl.wait(re.escape(k), 5)
        lats.append(round((t1 - t0) * 1000, 4))
        time.sleep(random.uniform(0.005, 0.015))
    return lats


def pct(v, p):
    s = sorted(v)
    return s[min(len(s) - 1, int(round(p / 100 * (len(s) - 1))))]


def t_latency(impl):
    out = {}
    for mode in ('quiet', 'flood'):
        impl.start('lat', '-s', 'L', '-x', '120', '-y', '39', 'stty -icanon -echo; exec cat')
        if mode == 'flood':
            impl.c('split-window', '-d', '-t', 'L:0', 'while :; do seq 1 100000; done')
        cl = Client(impl, 'L')
        cl.wait(rb'\[L\]', 10)
        time.sleep(1)
        thr0 = cg_throttle()
        a = impl.snap(); ca = cl.snap(); t0 = time.perf_counter()
        lats = key_latency(impl, cl)
        b = impl.snap(); cb = cl.snap(); t1 = time.perf_counter()
        thr1 = cg_throttle()
        out[mode] = {'p50_ms': pct(lats, 50), 'p90_ms': pct(lats, 90), 'p99_ms': pct(lats, 99), 'max_ms': max(lats),
                     'mean_ms': round(sum(lats) / len(lats), 4), 'samples_ms': lats,
                     'server': delta(a, b), 'client': delta(ca, cb), 'secs': round(t1 - t0, 3),
                     'server_cpu_pct': round(100 * (b['cpu'] - a['cpu']) / (t1 - t0), 1),
                     'throttled_ms': (thr1[1] - thr0[1]) / 1000}
        cl.close()
        impl.kill()
    return out


def timed_loop(impl, n, argv_fn):
    ts = []
    a = impl.snap()
    for i in range(n):
        argv = argv_fn(i)
        t0 = time.perf_counter()
        for args in argv:
            r = subprocess.run([impl.bin, '-L', impl.sock, *args], stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
            if r.returncode:
                raise RuntimeError(f'{impl.name} {args}: {r.stderr}')
        ts.append((time.perf_counter() - t0) * 1000)
    b = impl.snap()
    return {'n': n, 'mean_ms': round(sum(ts) / n, 4), 'p50_ms': round(pct(ts, 50), 4), 'p99_ms': round(pct(ts, 99), 4),
            'total_s': round(sum(ts) / 1000, 3), 'server': delta(a, b)}


def t_commands(impl):
    out = {}
    impl.start('cmd', '-s', 'c', '-x', '120', '-y', '40', 'seq 1 3000; exec cat')
    for i in range(9):
        impl.c('new-window', '-d', '-t', 'c:', 'exec cat')
    time.sleep(1)
    out['display_pane_id'] = timed_loop(impl, 1000, lambda i: [['display', '-p', '-t', 'c:0', '#{pane_id}']])
    out['list_windows_a'] = timed_loop(impl, 1000, lambda i: [['list-windows', '-a']])
    out['capture_pane_1000'] = timed_loop(impl, 1000, lambda i: [['capture-pane', '-p', '-S', '-1000', '-t', 'c:0']])
    out['send_keys'] = timed_loop(impl, 1000, lambda i: [['send-keys', '-t', 'c:1', 'x']])
    out['new_kill_window_cycle'] = timed_loop(impl, 300, lambda i: [['new-window', '-d', '-t', 'c:', '-n', 'tmpw', 'exec cat'],
                                                                     ['kill-window', '-t', 'c:tmpw']])
    impl.kill()
    st = []
    for i in range(20):
        st.append(round(impl.start('st', '-s', 'z', '-x', '80', '-y', '24') * 1000, 3))
        impl.kill()
    out['server_start_new_d_ms'] = {'samples': st, 'p50_ms': pct(st, 50), 'max_ms': max(st)}
    impl.start('att', '-s', 'qq', '-x', '120', '-y', '39')
    at = []
    for i in range(20):
        cl = Client(impl, 'qq')
        t1 = cl.wait(rb'\[qq\]', 10)
        at.append(round((t1 - cl.t_spawn) * 1000, 3))
        cl.close()
        time.sleep(0.2)
    out['attach_ms'] = {'samples': at, 'p50_ms': pct(at, 50), 'max_ms': max(at)}
    impl.kill()
    return out


def t_heavy(impl):
    out = {}
    impl.start('heavy', '-s', 'h', '-x', '120', '-y', '39', 'exec cat')
    impl.c('set', '-g', 'history-limit', '50000')
    impl.c('new-window', '-t', 'h:', '-n', 'big', f'seq 1 60000; echo END""MARK; exec cat')
    impl.wait_capture('h:big', 'ENDMARK', interval=0.05)
    cl = Client(impl, 'h:big')
    cl.wait(rb'\[h\]', 10)
    time.sleep(0.5)
    cm_cmd, cm_vis = [], []
    a = impl.snap()
    for i in range(20):
        cl.mark()
        t0 = time.perf_counter()
        impl.c('copy-mode', '-t', 'h:big')
        t1 = time.perf_counter()
        t2 = cl.wait(rb'\[0/\d+\]', 10)
        cm_cmd.append(round((t1 - t0) * 1000, 3)); cm_vis.append(round((t2 - t0) * 1000, 3))
        impl.c('send-keys', '-t', 'h:big', '-X', 'cancel')
        time.sleep(0.15)
    b = impl.snap()
    out['copy_mode_enter'] = {'cmd_p50_ms': pct(cm_cmd, 50), 'visible_p50_ms': pct(cm_vis, 50), 'visible_max_ms': max(cm_vis),
                              'cmd_samples': cm_cmd, 'visible_samples': cm_vis, 'server': delta(a, b)}
    for sname, pat in (('search_found_top', '10007'), ('search_notfound', 'qzqzq'), ('search_text_found_top', '10007')):
        xs = []
        a = impl.snap()
        for i in range(10):
            impl.c('copy-mode', '-t', 'h:big')
            time.sleep(0.05)
            cmd = 'search-backward-text' if 'text' in sname else 'search-backward'
            t0 = time.perf_counter()
            impl.c('send-keys', '-t', 'h:big', '-X', cmd, pat)
            xs.append(round((time.perf_counter() - t0) * 1000, 3))
            impl.c('send-keys', '-t', 'h:big', '-X', 'cancel')
            time.sleep(0.1)
        b = impl.snap()
        out[sname] = {'p50_ms': pct(xs, 50), 'max_ms': max(xs), 'samples': xs, 'server': delta(a, b)}
    cl.close()
    time.sleep(0.3)
    # resize with 50000-line history (no client: window-size is driven only by resize-window)
    a = impl.snap()
    t0 = time.perf_counter()
    for i in range(100):
        impl.c('resize-window', '-t', 'h:big', '-x', '200', '-y', '60')
        impl.c('resize-window', '-t', 'h:big', '-x', '80', '-y', '24')
    impl.c('display', '-p', '-t', 'h:big', '#{window_width}')
    t1 = time.perf_counter()
    b = impl.snap()
    out['resize_100_cycles'] = {'total_s': round(t1 - t0, 3), 'per_cycle_ms': round((t1 - t0) * 10, 3), 'server': delta(a, b),
                                'history_size_after': int(impl.c('display', '-p', '-t', 'h:big', '#{history_size}'))}
    impl.kill()
    # choose-tree with 40 windows
    impl.start('tree', '-s', 't', '-x', '120', '-y', '39', 'exec cat')
    for i in range(39):
        impl.c('new-window', '-d', '-t', 't:', 'exec cat')
    cl = Client(impl, 't:0')
    cl.wait(rb'\[t\]', 10)
    time.sleep(0.5)
    xs, vs = [], []
    a = impl.snap()
    for i in range(20):
        cl.mark()
        t0 = time.perf_counter()
        impl.c('choose-tree', '-t', 't:0')
        t1 = time.perf_counter()
        t2 = cl.wait(rb'40 windows', 10)
        xs.append(round((t1 - t0) * 1000, 3)); vs.append(round((t2 - t0) * 1000, 3))
        impl.c('send-keys', '-t', 't:0', 'q')
        time.sleep(0.2)
    b = impl.snap()
    out['choose_tree_40'] = {'cmd_p50_ms': pct(xs, 50), 'visible_p50_ms': pct(vs, 50), 'visible_max_ms': max(vs),
                             'visible_samples': vs, 'server': delta(a, b)}
    cl.close()
    impl.kill()
    # 5 attached clients receiving a flood
    impl.start('flood5', '-s', 'f', '-x', '120', '-y', '39', 'exec cat')
    cls = [Client(impl, 'f') for _ in range(5)]
    for c in cls:
        c.wait(rb'\[f\]', 10)
    time.sleep(0.5)
    for c in cls:
        c.mark()
    w = impl.popen('wait-for', 'fdone')
    time.sleep(0.1)
    thr0 = cg_throttle()
    a = impl.snap(); cas = [c.snap() for c in cls]
    t0 = time.perf_counter()
    impl.c('new-window', '-t', 'f:', '-n', 'fl', f'cat {DATA}/text50.txt; {impl.bin} -L {impl.sock} wait-for -S fdone; echo END""MARK; exec cat')
    w.wait(600)
    ends = [c.wait(rb'ENDMARK', 120) for c in cls]
    t1 = max(ends)
    b = impl.snap(); cbs = [c.snap() for c in cls]
    thr1 = cg_throttle()
    ccpu = sum(cb['cpu'] - ca['cpu'] for ca, cb in zip(cas, cbs))
    out['flood_5_clients'] = {'secs': round(t1 - t0, 4), 'server': delta(a, b), 'clients_cpu_s': round(ccpu, 3),
                              'client_bytes': [c.total for c in cls], 'mem_after': mem_view(b),
                              'throttled_ms': (thr1[1] - thr0[1]) / 1000}
    for c in cls:
        c.close()
    impl.kill()
    return out


# ---------------------------------------------------------------- driver
def main():
    outp, groups = sys.argv[1], sys.argv[2].split(',')
    impls = [Impl(*a.split('=', 1)) for a in sys.argv[3:]]
    if 'all' in groups:
        groups = ['idle', 'memory', 'throughput', 'latency', 'commands', 'heavy']
    make_data()
    res = {'meta': {'start': time.time(), 'reps': REPS, 'idle_secs': IDLE_SECS, 'impls': {i.name: i.bin for i in impls},
                    'uname': list(os.uname()), 'nproc': os.cpu_count(),
                    'cpu_max': open('/sys/fs/cgroup/cpu.max').read().strip()},
           'raw': {i.name: {} for i in impls}}

    def save():
        res['meta']['end'] = time.time()
        with open(outp + '.tmp', 'w') as f:
            json.dump(res, f, indent=1)
        os.replace(outp + '.tmp', outp)

    for g in groups:
        for rep in range(REPS):
            order = impls if rep % 2 == 0 else impls[::-1]
            if g == 'idle':
                # idle runs both implementations at the same time: same 30 s window, negligible load
                for kind in ('a', 'b', 'c'):
                    log(f'idle {kind} rep {rep}')
                    box = {}
                    ths = [threading.Thread(target=lambda i=i: box.__setitem__(i.name, idle_scenario(i, kind))) for i in order]
                    for t in ths:
                        t.start()
                    for t in ths:
                        t.join()
                    for i in impls:
                        res['raw'][i.name].setdefault(f'idle_{kind}', []).append(box.get(i.name, {'error': 'failed'}))
                    save()
                continue
            for i in order:
                log(f'{g} rep {rep} {i.name}')
                fn = {'memory': t_memory, 'throughput': t_throughput, 'latency': t_latency,
                      'commands': t_commands, 'heavy': t_heavy}[g]
                try:
                    r = fn(i)
                except Exception as e:  # keep going; record the failure
                    log(f'ERROR {g} {i.name}: {e!r}')
                    r = {'error': repr(e)}
                    try:
                        i.kill()
                    except Exception:
                        pass
                res['raw'][i.name].setdefault(g, []).append(r)
                save()
    log('done')


if __name__ == '__main__':
    main()
