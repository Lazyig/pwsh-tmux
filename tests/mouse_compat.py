"""Compare mouse behaviour (`set -g mouse on`) with a real tmux.

Runs on Linux (or WSL) with Python 3.6+. Each scenario starts a private server
for both programs, attaches a client inside a pseudo terminal, writes the same
SGR mouse sequences to it and records state: which key events fired, the
active pane and window, copy-mode state, paste buffers and the bytes an
application in the pane received.

  python3 tests/mouse_compat.py --reference /path/to/tmux \
      --candidate target/debug/pwsh-tmux [--only NAME] [--verbose]

Exit status is 0 when every scenario matched.
"""

import argparse
import fcntl
import json
import os
import pty
import select
import shutil
import struct
import subprocess
import sys
import tempfile
import termios
import threading
import time
import uuid

COLS, ROWS = 80, 24
FIXTURE = r'''
import os, sys, time, select
mode, log = sys.argv[1], sys.argv[2]
lines = ["alpha beta gamma-delta epsilon", "second line here", "x=1 y=22 zeta",
         "fourth row of text", "fifth"]
out = sys.stdout
if mode in ("history", "app", "alt"):
    for i in range(60):
        out.write("history %02d filler words\r\n" % i)
if mode == "alt":
    out.write("\x1b[?1049h\x1b[H")
if mode == "app":
    out.write("\x1b[?1000h\x1b[?1002h\x1b[?1006h")
out.write("\x1b[H\x1b[2J" if mode in ("app", "alt") else "")
for l in lines:
    out.write(l + "\r\n")
out.flush()
import tty
try:
    tty.setraw(0)
except Exception:
    pass
fd = os.open(log, os.O_WRONLY | os.O_CREAT | os.O_APPEND)
while True:
    r, _, _ = select.select([0], [], [], 1)
    if r:
        data = os.read(0, 4096)
        if not data:
            break
        os.write(fd, data)
'''


def sgr(button, x, y, release=False, motion=False):
    """SGR mouse report; x/y are 0-based screen cells."""
    code = button
    if motion:
        code += 32
    return "\x1b[<%d;%d;%d%s" % (code, x + 1, y + 1, "m" if release else "M")


class Impl:
    def __init__(self, name, command, workdir):
        self.name = name
        self.command = command
        self.socket = "mc-" + uuid.uuid4().hex[:12]
        self.workdir = workdir
        self.master = None
        self.proc = None
        self.output = bytearray()
        self.reader = None

    def env(self):
        env = dict(os.environ)
        for key in ("TMUX", "TMUX_PANE", "PWSH_TMUX", "PWSH_TMUX_PANE", "PWSH_TMUX_SOCKET", "PWSH_TMUX_SOCKET_PATH"):
            env.pop(key, None)
        env.update(TERM="xterm-256color", LC_ALL="C.UTF-8", SHELL="/bin/sh", PWSH_TMUX_SHELL="/bin/sh")
        return env

    def run(self, *args, check=False):
        r = subprocess.run(self.command + ["-L", self.socket, "-f", "/dev/null"] + list(args),
                           stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=self.env(), timeout=20)
        out = r.stdout.decode("utf-8", "replace").rstrip("\r\n")
        if check and r.returncode:
            raise RuntimeError("%s %s: %s" % (self.name, args, r.stderr.decode()))
        return out

    def fixture(self, mode):
        path = os.path.join(self.workdir, "fixture.py")
        with open(path, "w") as f:
            f.write(FIXTURE)
        return "python3 %s %s %s" % (path, mode, self.log_path())

    def log_path(self):
        return os.path.join(self.workdir, "%s-app.log" % self.name)

    def app_log(self):
        try:
            with open(self.log_path(), "rb") as f:
                return f.read().decode("utf-8", "replace")
        except OSError:
            return ""

    def start(self, mode):
        open(self.log_path(), "w").close()
        self.run("new-session", "-d", "-s", "m", "-x", str(COLS), "-y", str(ROWS - 1), self.fixture(mode), check=True)
        self.run("set", "-g", "mouse", "on")
        self.run("set", "-g", "status-right", "RIGHT")
        self.run("set", "-g", "status-left", "[m] ")
        self.run("set", "-g", "default-command", self.fixture("plain"))
        self.run("set", "-gw", "automatic-rename", "off")
        self.run("rename-window", "-t", "m:0", "w0")
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))

        def child_setup():
            os.setsid()
            fcntl.ioctl(0, termios.TIOCSCTTY, 0)

        self.proc = subprocess.Popen(self.command + ["-L", self.socket, "-f", "/dev/null", "attach", "-t", "m"],
                                     stdin=slave, stdout=slave, stderr=slave, env=self.env(),
                                     preexec_fn=child_setup, close_fds=True)
        os.close(slave)
        self.master = master
        self.reader = threading.Thread(target=self._read, daemon=True)
        self.reader.start()
        deadline = time.time() + 10
        while time.time() < deadline:
            if self.run("list-clients", "-F", "#{client_name}"):
                break
            time.sleep(0.05)
        else:
            raise RuntimeError(self.name + ": client did not attach")
        time.sleep(0.5)

    def _read(self):
        while True:
            try:
                r, _, _ = select.select([self.master], [], [], 0.2)
                if r:
                    data = os.read(self.master, 65536)
                    if not data:
                        return
                    self.output.extend(data)
            except OSError:
                return

    def send(self, data):
        os.write(self.master, data.encode())

    def stop(self):
        self.run("kill-server")
        if self.proc:
            try:
                self.proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.proc.kill()
        if self.master is not None:
            try:
                os.close(self.master)
            except OSError:
                pass


STATE = ("#{window_index}|#{window_panes}|#{pane_index}|#{pane_in_mode}|#{pane_mode}|"
         "#{scroll_position}|#{copy_cursor_x},#{copy_cursor_y}|#{selection_present}|#{client_key_table}")


def state(impl):
    out = {
        "state": impl.run("display-message", "-p", "-t", "m", STATE),
        "windows": impl.run("list-windows", "-t", "m", "-F", "#{window_index}:#{window_name}:#{window_active}"),
        "panes": impl.run("list-panes", "-t", "m", "-F", "#{pane_index}:#{pane_left},#{pane_top},#{pane_width}x#{pane_height}:#{pane_active}"),
        "buffer": impl.run("show-buffer") if impl.run("list-buffers") else "",
        "log": impl.run("show-options", "-gqv", "@log"),
        "app": impl.app_log(),
    }
    if os.environ.get("MOUSE_COMPAT_SCREEN"):
        out["screen"] = impl.run("capture-pane", "-p", "-t", "m")
    sel = impl.run("display-message", "-p", "-t", "m", "#{selection_present}")
    if sel == "1":
        out["selection"] = impl.run("display-message", "-p", "-t", "m",
                                    "#{selection_start_x},#{selection_start_y}-#{selection_end_x},#{selection_end_y}")
    return out


LOG_BINDINGS = [
    "MouseDown1Pane", "MouseUp1Pane", "MouseDrag1Pane", "MouseDragEnd1Pane", "SecondClick1Pane",
    "DoubleClick1Pane", "TripleClick1Pane", "WheelUpPane", "WheelDownPane", "MouseDown3Pane",
    "MouseDown1Border", "MouseUp1Border", "MouseDrag1Border", "MouseDragEnd1Border", "WheelUpBorder",
    "MouseDown1Status", "MouseDown1StatusLeft", "MouseDown1StatusRight", "MouseDown1StatusDefault",
    "MouseUp1Status", "MouseUp1StatusLeft", "MouseUp1StatusRight", "MouseUp1StatusDefault",
    "WheelUpStatus", "WheelUpStatusLeft", "WheelUpStatusRight", "WheelUpStatusDefault",
    "MouseDrag1Status", "MouseDragEnd1Status", "MouseDragEnd1StatusDefault",
    "M-MouseDown1Pane", "C-MouseDown1Pane", "S-MouseDown1Pane",
    "MouseDown2Pane", "MouseUp2Pane", "MouseDown3Status", "MouseUp3Pane",
]


def log_all(impl, fmt="#{mouse_x},#{mouse_y}"):
    impl.run("set", "-g", "@log", "")
    for key in LOG_BINDINGS:
        impl.run("bind", "-n", key, "set", "-gaF", "@log", "%s@%s " % (key, fmt))


# Each step is (data-or-callable, delay seconds). Scenarios return a list of steps plus setup.
def click(x, y, b=0, delay=0.05):
    return [(sgr(b, x, y), 0.02), (sgr(b, x, y, release=True), delay)]


def drag(x0, y0, x1, y1, b=0, steps=3, delay=0.3):
    out = [(sgr(b, x0, y0), 0.05)]
    for i in range(1, steps + 1):
        x = x0 + (x1 - x0) * i // steps
        y = y0 + (y1 - y0) * i // steps
        out.append((sgr(b, x, y, motion=True), 0.05))
    out.append((sgr(b, x1, y1, release=True), delay))
    return out


def wheel(x, y, up=True, n=1, delay=0.2):
    return [(sgr(64 if up else 65, x, y), 0.03) for _ in range(n)] + [("", delay)]


def keys(s, delay=0.3):
    return [(s, delay)]


SCENARIOS = {}


def scenario(name, mode="plain"):
    def wrap(fn):
        SCENARIOS[name] = (mode, fn)
        return fn
    return wrap


# ---- key event classification -------------------------------------------------------
@scenario("events-single-click")
def _(impl):
    log_all(impl)
    return click(5, 3, delay=0.5)


@scenario("events-double-click")
def _(impl):
    log_all(impl)
    return click(5, 3) + click(5, 3, delay=0.6)


@scenario("events-double-click-moved")
def _(impl):
    log_all(impl)
    return click(5, 3) + click(9, 4, delay=0.6)


@scenario("events-triple-click")
def _(impl):
    log_all(impl)
    return click(5, 3) + click(5, 3) + click(5, 3, delay=0.6)


@scenario("events-quadruple-click")
def _(impl):
    log_all(impl)
    return click(5, 3) + click(5, 3) + click(5, 3) + click(5, 3, delay=0.6)


@scenario("events-slow-clicks")
def _(impl):
    log_all(impl)
    return click(5, 3, delay=0.5) + click(5, 3, delay=0.5)


@scenario("events-drag")
def _(impl):
    log_all(impl)
    return drag(2, 2, 12, 4)


@scenario("events-drag-same-cell")
def _(impl):
    log_all(impl)
    return [(sgr(0, 4, 4), 0.05), (sgr(0, 4, 4, motion=True), 0.05), (sgr(0, 4, 4, motion=True), 0.05),
            (sgr(0, 6, 4, motion=True), 0.05), (sgr(0, 6, 4, release=True), 0.4)]


@scenario("events-right-and-middle")
def _(impl):
    log_all(impl)
    return click(5, 3, b=2) + click(5, 3, b=1, delay=0.4)


@scenario("events-modifiers")
def _(impl):
    log_all(impl)
    return click(5, 3, b=8, delay=0.4) + click(5, 3, b=16, delay=0.4) + click(5, 3, b=4, delay=0.4)


@scenario("events-wheel")
def _(impl):
    log_all(impl)
    return wheel(5, 3, n=2) + wheel(5, 3, up=False)


@scenario("events-status-areas")
def _(impl):
    log_all(impl, "#{mouse_x},#{mouse_y},#{mouse_status_line},#{mouse_status_range}")
    impl.run("new-window", "-d")
    return (click(1, ROWS - 1, delay=0.4) + click(6, ROWS - 1, delay=0.4) + click(40, ROWS - 1, delay=0.4)
            + click(COLS - 2, ROWS - 1, delay=0.4) + wheel(1, ROWS - 1) + wheel(40, ROWS - 1) + wheel(COLS - 2, ROWS - 1))


@scenario("events-status-drag")
def _(impl):
    log_all(impl)
    return drag(40, ROWS - 1, 50, ROWS - 1)


@scenario("events-border")
def _(impl):
    impl.run("split-window", "-h", "-t", "m")
    log_all(impl)
    border = int(impl.run("display-message", "-p", "-t", "m:0.0", "#{pane_width}"))
    return click(border, 5, delay=0.4) + drag(border, 5, border + 3, 5) + wheel(border, 5)


@scenario("events-drag-pane-to-status")
def _(impl):
    log_all(impl)
    return drag(5, 5, 10, ROWS - 1)


@scenario("events-prefix-then-click")
def _(impl):
    log_all(impl)
    return keys("\x02", 0.1) + click(5, 3, delay=0.5)


@scenario("format-mouse-word-line")
def _(impl):
    impl.run("set", "-g", "@log", "")
    impl.run("bind", "-n", "MouseDown1Pane", "set", "-gaF", "@log",
             "[#{mouse_x},#{mouse_y}|#{mouse_word}|#{mouse_line}|#{mouse_hyperlink}]")
    return click(2, 0, delay=0.4) + click(18, 0, delay=0.4) + click(6, 2, delay=0.4) + click(40, 1, delay=0.4) + click(5, 10, delay=0.4)


# ---- default bindings ---------------------------------------------------------------
@scenario("default-click-selects-pane")
def _(impl):
    impl.run("split-window", "-h", "-t", "m")
    return click(5, 5, delay=0.4)


@scenario("default-drag-border")
def _(impl):
    impl.run("split-window", "-h", "-t", "m")
    border = int(impl.run("display-message", "-p", "-t", "m:0.0", "#{pane_width}"))
    return drag(border, 5, border - 6, 5)


@scenario("default-drag-border-horizontal")
def _(impl):
    impl.run("split-window", "-v", "-t", "m")
    border = int(impl.run("display-message", "-p", "-t", "m:0.0", "#{pane_height}"))
    return drag(10, border, 10, border + 4)


@scenario("default-wheel-enters-copy", mode="history")
def _(impl):
    return wheel(5, 5, n=2)


@scenario("default-wheel-up-down-exits", mode="history")
def _(impl):
    return wheel(5, 5, n=1) + wheel(5, 5, up=False, n=1, delay=0.4)


@scenario("default-wheel-down-with-selection", mode="history")
def _(impl):
    return wheel(5, 5, n=2) + drag(2, 3, 8, 3, delay=0.2) + wheel(5, 5, up=False, n=3, delay=0.4)


@scenario("default-drag-copies")
def _(impl):
    return drag(0, 0, 10, 1, delay=0.5)


@scenario("default-drag-copies-backwards")
def _(impl):
    return drag(12, 2, 3, 0, delay=0.5)


@scenario("default-double-click-word")
def _(impl):
    return click(8, 0) + click(8, 0, delay=0.8)


@scenario("default-double-click-separator-word")
def _(impl):
    return click(19, 0) + click(19, 0, delay=0.8)


@scenario("default-triple-click-line")
def _(impl):
    return click(8, 1) + click(8, 1) + click(8, 1, delay=0.8)


@scenario("default-middle-click-pastes")
def _(impl):
    impl.run("set-buffer", "pasted-text")
    return click(5, 5, b=1, delay=0.5)


@scenario("default-right-click-pane-menu-split")
def _(impl):
    return click(5, 5, b=2, delay=0.4) + keys("h", 0.6)


@scenario("default-right-click-pane-menu-escape")
def _(impl):
    return click(5, 5, b=2, delay=0.4) + keys("\x1b", 0.6)


@scenario("default-right-click-menu-copy-word")
def _(impl):
    return click(8, 0, b=2, delay=0.4) + keys("c", 0.6)


@scenario("default-right-click-menu-copy-line")
def _(impl):
    return click(8, 1, b=2, delay=0.4) + keys("l", 0.6)


@scenario("default-right-click-menu-type-word")
def _(impl):
    return click(8, 0, b=2, delay=0.4) + keys("\x19", 0.6)


@scenario("default-right-click-menu-search-word", mode="history")
def _(impl):
    return click(2, 1, b=2, delay=0.4) + keys("\x12", 0.6)


@scenario("default-meta-right-click-pane-menu")
def _(impl):
    return click(5, 5, b=2 + 8, delay=0.4) + keys("v", 0.6)


@scenario("default-right-click-window-menu")
def _(impl):
    impl.run("new-window", "-d")
    return click(6, ROWS - 1, b=2, delay=0.4) + keys("W", 0.6)


@scenario("default-right-click-window-menu-disabled-swap")
def _(impl):
    return click(6, ROWS - 1, b=2, delay=0.4) + keys("r", 0.4) + keys("\x1b", 0.4)


@scenario("default-right-click-session-menu")
def _(impl):
    return click(1, ROWS - 1, b=2, delay=0.4) + keys("w", 0.6)


@scenario("default-right-click-status-right")
def _(impl):
    return click(COLS - 2, ROWS - 1, b=2, delay=0.4) + keys("w", 0.6)


@scenario("default-click-status-window")
def _(impl):
    impl.run("new-window", "-d")
    return click(10, ROWS - 1, delay=0.5)


@scenario("default-wheel-status-windows")
def _(impl):
    impl.run("new-window", "-d")
    impl.run("new-window", "-d")
    return wheel(40, ROWS - 1, up=False, n=2, delay=0.4) + wheel(40, ROWS - 1, up=True, delay=0.4)


@scenario("default-click-status-left-right")
def _(impl):
    impl.run("new-window", "-d")
    return click(1, ROWS - 1, delay=0.4) + click(COLS - 2, ROWS - 1, delay=0.4)


# ---- copy mode ----------------------------------------------------------------------
@scenario("copy-click-does-not-move", mode="history")
def _(impl):
    impl.run("copy-mode", "-t", "m")
    return click(10, 5, delay=0.4)


@scenario("copy-click-keeps-selection", mode="history")
def _(impl):
    impl.run("copy-mode", "-t", "m")
    impl.run("send-keys", "-t", "m", "-X", "begin-selection")
    impl.run("send-keys", "-t", "m", "-X", "cursor-left")
    impl.run("send-keys", "-t", "m", "-X", "cursor-up")
    return click(10, 5, delay=0.4)


@scenario("copy-drag-copies-and-exits", mode="history")
def _(impl):
    impl.run("copy-mode", "-t", "m")
    return drag(2, 3, 14, 4, delay=0.5)


@scenario("copy-drag-vi", mode="history")
def _(impl):
    impl.run("set", "-gw", "mode-keys", "vi")
    impl.run("copy-mode", "-t", "m")
    return drag(14, 4, 2, 3, delay=0.5)


@scenario("copy-wheel-scrolls", mode="history")
def _(impl):
    impl.run("copy-mode", "-t", "m")
    return wheel(10, 5, n=3, delay=0.3) + wheel(10, 5, up=False, delay=0.3)


@scenario("copy-wheel-down-to-bottom-stays", mode="history")
def _(impl):
    impl.run("copy-mode", "-t", "m")
    return wheel(10, 5, n=1, delay=0.3) + wheel(10, 5, up=False, n=3, delay=0.3)


@scenario("copy-double-click", mode="history")
def _(impl):
    impl.run("copy-mode", "-t", "m")
    return click(10, 5) + click(10, 5, delay=0.8)


@scenario("copy-triple-click", mode="history")
def _(impl):
    impl.run("copy-mode", "-t", "m")
    return click(10, 5) + click(10, 5) + click(10, 5, delay=0.8)


@scenario("copy-right-click-menu-top", mode="history")
def _(impl):
    impl.run("copy-mode", "-t", "m")
    return click(10, 5, b=2, delay=0.4) + keys("<", 0.5)


@scenario("copy-middle-click", mode="history")
def _(impl):
    impl.run("set-buffer", "zzz")
    impl.run("copy-mode", "-t", "m")
    return click(10, 5, b=1, delay=0.5)


@scenario("copy-click-other-pane", mode="history")
def _(impl):
    impl.run("split-window", "-h", "-t", "m")
    impl.run("select-pane", "-t", "m:0.0")
    impl.run("copy-mode", "-t", "m:0.0")
    return click(60, 5, delay=0.4)


@scenario("copy-drag-at-bottom-edge", mode="history")
def _(impl):
    impl.run("copy-mode", "-t", "m")
    impl.run("send-keys", "-t", "m", "-X", "-N", "10", "scroll-up")
    # held at the bottom edge long enough to scroll to the end (50 ms per line)
    steps = drag(3, 5, 3, ROWS - 2, delay=0.5)
    return steps[:-1] + [("", 1.5)] + steps[-1:]


# ---- menus, modes and layouts -------------------------------------------------------
@scenario("menu-drag-to-item")
def _(impl):
    # press the right button, move onto "Horizontal Split" and release there
    return [(sgr(2, 30, 3), 0.3), (sgr(2, 31, 4, motion=True), 0.1), (sgr(2, 31, 13, motion=True), 0.1),
            (sgr(2, 31, 13, release=True), 0.6)]


@scenario("menu-drag-off-menu")
def _(impl):
    return [(sgr(2, 30, 3), 0.3), (sgr(2, 31, 6, motion=True), 0.1), (sgr(2, 70, 20, motion=True), 0.1),
            (sgr(2, 70, 20, release=True), 0.4)] + keys("h", 0.4)


@scenario("menu-stay-open")
def _(impl):
    impl.run("bind", "-n", "MouseDown3Pane", "display-menu", "-O", "-x", "M", "-y", "M", "First", "f",
             "set -g @log first", "Second", "s", "set -g @log second")
    return click(10, 5, b=2, delay=0.4) + click(12, 7, delay=0.5)


@scenario("menu-keyboard-then-click")
def _(impl):
    impl.run("bind", "-n", "F5", "display-menu", "-x", "5", "-y", "10", "First", "f", "set -g @log first",
             "Second", "s", "set -g @log second")
    return keys("\x1b[15~", 0.4) + click(8, 3, delay=0.4) + keys("s", 0.4)


@scenario("menu-keyboard-then-wheel")
def _(impl):
    impl.run("bind", "-n", "F5", "display-menu", "-x", "5", "-y", "10", "First", "f", "set -g @log first")
    return keys("\x1b[15~", 0.4) + wheel(8, 3, delay=0.4) + keys("f", 0.4)


@scenario("prefix-pane-menu-keyboard")
def _(impl):
    impl.run("split-window", "-h", "-t", "m")
    return keys("\x02", 0.1) + keys(">", 0.4) + keys("v", 0.5)


@scenario("prefix-window-menu-keyboard")
def _(impl):
    return keys("\x02", 0.1) + keys("<", 0.4) + keys("W", 0.5)


@scenario("tree-click-and-double-click")
def _(impl):
    impl.run("new-window", "-d", "-n", "two")
    impl.run("new-window", "-d", "-n", "three")
    impl.run("choose-tree", "-t", "m", "-w")
    return click(10, 3, delay=0.4) + click(10, 2) + click(10, 2, delay=0.8)


@scenario("tree-wheel")
def _(impl):
    impl.run("new-window", "-d", "-n", "two")
    impl.run("choose-tree", "-t", "m", "-w")
    return wheel(10, 3, up=False, n=2, delay=0.4) + keys("\r", 0.5)


@scenario("clock-click-exits")
def _(impl):
    impl.run("clock-mode", "-t", "m")
    return click(10, 5, delay=0.5)


@scenario("status-top-click")
def _(impl):
    impl.run("set", "-g", "status-position", "top")
    impl.run("new-window", "-d")
    log_all(impl, "#{mouse_x},#{mouse_y},#{mouse_status_line}")
    return click(10, 0, delay=0.4) + click(5, 4, delay=0.4)


@scenario("status-top-default-bindings")
def _(impl):
    impl.run("set", "-g", "status-position", "top")
    impl.run("new-window", "-d")
    impl.run("split-window", "-v", "-t", "m:0")
    return click(10, 0, delay=0.4) + click(5, 3, delay=0.4)


@scenario("status-user-range")
def _(impl):
    impl.run("set", "-g", "status-format[0]", "#[range=user|abc]USER#[norange] mid #[range=window|0]W0#[norange]#[align=right]#[range=right]R")
    log_all(impl, "#{mouse_status_range}")
    return click(1, ROWS - 1, delay=0.4) + click(6, ROWS - 1, delay=0.4) + click(12, ROWS - 1, delay=0.4) + click(COLS - 1, ROWS - 1, delay=0.4)


@scenario("status-right-edge")
def _(impl):
    impl.run("set", "-g", "status-format[0]", "#[range=window|0]W0#[norange]#[align=right]#[range=right]RR")
    log_all(impl, "#{mouse_status_range}")
    return (click(0, ROWS - 1, delay=0.4) + click(1, ROWS - 1, delay=0.4) + click(2, ROWS - 1, delay=0.4)
            + click(COLS - 3, ROWS - 1, delay=0.4) + click(COLS - 2, ROWS - 1, delay=0.4) + click(COLS - 1, ROWS - 1, delay=0.4))


@scenario("pane-border-status-click")
def _(impl):
    impl.run("set", "-g", "pane-border-status", "top")
    impl.run("split-window", "-v", "-t", "m")
    log_all(impl)
    top = int(impl.run("display-message", "-p", "-t", "m:0.1", "#{pane_top}"))
    return click(5, 0, delay=0.4) + click(5, top - 1, delay=0.4) + click(5, top, delay=0.4)


@scenario("zoomed-click")
def _(impl):
    impl.run("split-window", "-h", "-t", "m")
    impl.run("resize-pane", "-Z", "-t", "m")
    log_all(impl)
    return click(5, 5, delay=0.4) + click(60, 5, delay=0.4)


@scenario("app-any-motion", mode="app")
def _(impl):
    return [(sgr(3, 5, 3, motion=True), 0.1), (sgr(3, 6, 3, motion=True), 0.2)] + click(5, 3, delay=0.3)


@scenario("copy-e-wheel-exit-with-selection", mode="history")
def _(impl):
    impl.run("copy-mode", "-e", "-t", "m")
    impl.run("send-keys", "-t", "m", "-X", "-N", "3", "scroll-up")
    impl.run("send-keys", "-t", "m", "-X", "begin-selection")
    return wheel(10, 5, up=False, n=1, delay=0.4)


# ---- applications -------------------------------------------------------------------
@scenario("app-receives-mouse", mode="app")
def _(impl):
    return click(5, 3) + drag(5, 3, 9, 4, delay=0.2) + wheel(5, 3, delay=0.3) + click(5, 3, b=2, delay=0.3)


@scenario("app-double-click", mode="app")
def _(impl):
    return click(5, 3) + click(5, 3, delay=0.6)


@scenario("app-triple-click", mode="app")
def _(impl):
    return click(5, 3) + click(5, 3) + click(5, 3, delay=0.6)


@scenario("app-split-relative-coordinates", mode="app")
def _(impl):
    impl.run("split-window", "-h", "-d", "-t", "m", "-b")
    left = int(impl.run("display-message", "-p", "-t", "m:0.0", "#{pane_width}"))
    return click(left + 3, 2, delay=0.3) + click(left + 4, 2, delay=0.4)


@scenario("app-mouse-off-forwarded", mode="app")
def _(impl):
    impl.run("set", "-g", "mouse", "off")
    return click(5, 3) + wheel(5, 3, delay=0.4)


@scenario("alt-screen-wheel", mode="alt")
def _(impl):
    return wheel(5, 3, n=2, delay=0.4)


@scenario("plain-mouse-off-wheel", mode="history")
def _(impl):
    impl.run("set", "-g", "mouse", "off")
    return wheel(5, 3, n=2, delay=0.4) + click(5, 3, delay=0.3)


def run_scenario(name, impl_specs, verbose):
    mode, fn = SCENARIOS[name]
    results = {}
    for label, command in impl_specs:
        workdir = tempfile.mkdtemp(prefix="mouse-compat-")
        impl = Impl(label, command, workdir)
        try:
            impl.start(mode)
            steps = fn(impl)
            for data, delay in steps:
                if data:
                    impl.send(data)
                time.sleep(delay)
            time.sleep(0.2)
            results[label] = state(impl)
        except Exception as e:  # report and continue with other scenarios
            results[label] = {"error": repr(e)}
        finally:
            impl.stop()
            shutil.rmtree(workdir, ignore_errors=True)
    ref, cand = results["reference"], results["candidate"]
    same = ref == cand
    print("%s %s" % ("ok  " if same else "DIFF", name))
    if not same or verbose:
        for key in sorted(set(ref) | set(cand)):
            if ref.get(key) != cand.get(key) or verbose:
                print("   %-9s ref : %r" % (key, ref.get(key)))
                print("   %-9s cand: %r" % ("", cand.get(key)))
    return same, results


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--reference", required=True)
    ap.add_argument("--candidate", required=True)
    ap.add_argument("--only", action="append", help="scenario name prefix (repeatable)")
    ap.add_argument("--verbose", action="store_true")
    ap.add_argument("--json", help="write all results here")
    args = ap.parse_args()
    specs = [("reference", [args.reference]), ("candidate", [os.path.abspath(args.candidate)])]
    names = [n for n in SCENARIOS if not args.only or any(n.startswith(o) for o in args.only)]
    failed, everything = [], {}
    for name in names:
        same, results = run_scenario(name, specs, args.verbose)
        everything[name] = results
        if not same:
            failed.append(name)
    if args.json:
        with open(args.json, "w") as f:
            json.dump(everything, f, indent=1, ensure_ascii=False)
    print("\n%d/%d scenarios match" % (len(names) - len(failed), len(names)))
    if failed:
        print("differ: " + " ".join(failed))
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
