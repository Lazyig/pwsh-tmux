"""Exercise real Unix UID boundaries without creating accounts or installing files.

Run as root in a disposable Unix environment:
  python3 tests/security_multiuser.py /absolute/path/to/pwsh-tmux
All server state is confined to a fresh temporary directory.
"""
import argparse
import json
import os
from pathlib import Path
import select
import shutil
import socket
import struct
import subprocess
import sys
import tempfile
import time


def wire_client(endpoint):
    """A restricted OS user claiming a terminal and sending raw protocol input."""
    stream = socket.socket(socket.AF_UNIX)
    stream.settimeout(5)
    stream.connect(endpoint)

    def send(kind, payload):
        stream.sendall(struct.pack('<BI', kind, len(payload)) + payload)

    def args(words):
        return b''.join(word.encode() + b'\0' for word in words)

    def read_exact(n):
        data = b''
        while len(data) < n:
            part = stream.recv(n - len(data))
            if not part:
                raise RuntimeError('unexpected protocol EOF')
            data += part
        return data

    # A claimed pid/tty is not an authenticated OS identity.
    send(5, args(['80', '24', '1', '/tmp', 'xterm', '1', '', '0', '']))
    send(1, args(['attach-session', '-t', 'shared']))
    while True:
        kind, size = struct.unpack('<BI', read_exact(5))
        payload = read_exact(size)
        if kind == 13:
            break
        if kind in (11, 12):
            raise RuntimeError('wire attach failed: ' + repr(payload))
    send(1, args(['refresh-client', '-f', '!read-only']))
    send(3, b'echo WIRE_FORBIDDEN\r')
    send(3, b'\x1b[200~echo PASTE_FORBIDDEN\r\x1b[201~')
    send(3, b'\x1b[777~\x02c\x02%\x1b[<0;2;2M')
    send(1, args(['attach-session', '-t', 'shared']))
    send(3, b'echo REATTACH_FORBIDDEN\r')
    # Drain output so the peer cannot be disconnected for output backpressure.
    stream.settimeout(0.2)
    try:
        while stream.recv(65536):
            pass
    except socket.timeout:
        pass
    stream.close()
    print('wire input submitted')


class Audit:
    def __init__(self, binary):
        self.binary = str(Path(binary).resolve())
        self.base = Path(tempfile.mkdtemp(prefix='pt-security-'))
        self.base.chmod(0o755)
        self.runtime = self.base / 'runtime'
        self.runtime.mkdir(mode=0o1777)
        self.runtime.chmod(0o1777)
        self.owner, self.reader, self.outsider = 20001, 20002, 20003
        self.endpoint = str(self.runtime / ('pwsh-tmux-%d' % self.owner) / 'audit')
        self.children = []
        self.cases = []
        for uid in (self.owner, self.reader, self.outsider):
            home = self.base / str(uid)
            home.mkdir(mode=0o700)
            os.chown(str(home), uid, uid)

    def demote(self, uid):
        def apply():
            os.setgroups([])
            os.setgid(uid)
            os.setuid(uid)
        return apply

    def env(self, uid):
        return dict(PATH='/usr/bin:/bin', HOME=str(self.base / str(uid)),
                    XDG_CONFIG_HOME=str(self.base / str(uid) / 'config'),
                    PWSH_TMUX_TMPDIR=str(self.runtime), PWSH_TMUX_SOCKET='audit',
                    PWSH_TMUX_SHELL='/bin/sh', SHELL='/bin/sh', TERM='xterm-256color')

    def command(self, uid, *args, **kwargs):
        prefix = [] if uid == self.owner else ['-S', self.endpoint]
        return subprocess.Popen([self.binary] + prefix + list(args),
                                env=self.env(uid), preexec_fn=self.demote(uid), **kwargs)

    def run(self, uid, *args, **kwargs):
        expected = kwargs.pop('expected', True)
        proc = self.command(uid, *args, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        output = proc.communicate(timeout=10)[0].decode('utf-8', 'replace')
        if expected is not None:
            assert (proc.returncode == 0) == expected, (uid, args, proc.returncode, output)
        return output

    def check(self, name, condition):
        assert condition, name
        self.cases.append(name)

    def control(self, uid):
        proc = self.command(uid, '-C', 'attach-session', '-t', 'shared',
                            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.children.append(proc)
        self.until(proc, '%session-changed')
        return proc

    def until(self, proc, marker):
        output = ''
        deadline = time.monotonic() + 5
        while marker not in output:
            assert time.monotonic() < deadline, ('control timeout', marker, output)
            if select.select([proc.stdout], [], [], 0.05)[0]:
                chunk = os.read(proc.stdout.fileno(), 65536)
                assert chunk, ('control EOF', marker, output)
                output += chunk.decode('utf-8', 'replace')
        return output

    def control_command(self, proc, line):
        marker = 'BARRIER_%d' % len(self.cases)
        proc.stdin.write((line + "\ndisplay-message -p '" + marker + " #{client_readonly}'\n").encode())
        proc.stdin.flush()
        return self.until(proc, marker)

    def execute(self):
        owner, reader = self.owner, self.reader
        self.run(owner, 'new-session', '-d', '-s', 'shared', '-x', '100', '-y', '30')
        self.check('unshared endpoint refuses other UID', bool(self.run(reader, 'ls', expected=False)))
        self.run(owner, 'server-access', '-ar', str(reader))
        self.check('read-only UID can list', 'shared:' in self.run(reader, 'ls'))
        self.check('unlisted UID refused', 'not allowed' in self.run(self.outsider, 'ls', expected=False))
        marker = self.base / str(owner) / 'forbidden-job'
        shell = 'touch ' + str(marker)
        for argv in [
            ['run-shell', shell], ['new-window', '-d'], ['set-environment', '-g', 'INJECT', '1'],
            ['INJECT=1'], ['capture-pane'], ['attach-session', '-d'],
            ['server-access', '-w', str(reader)], ['switch-client', '-r'],
        ]:
            self.check('denied ' + ' '.join(argv[:2]), 'read-only' in self.run(reader, *argv, expected=False))
        self.run(reader, 'display-message', '-p', '#(' + shell + ')')
        self.run(reader, 'ls', '-F', '#(' + shell + ')')
        self.check('query formats did not start jobs', not marker.exists())
        batch = []
        for n in range(16):
            if n:
                batch.append(';')
            batch += ['display-message', '-p', '#{p10000:#{l:x}}' * 100]
        self.check('combined reply amplification refused', 'output limit exceeded' in self.run(reader, *batch, expected=False))
        cache_fmt = '#(printf OWNER_CACHE_PROOF)'
        self.run(owner, 'display-message', '-p', cache_fmt)
        deadline = time.monotonic() + 5
        while self.run(owner, 'display-message', '-p', cache_fmt).strip() != 'OWNER_CACHE_PROOF':
            assert time.monotonic() < deadline, 'owner format job failed'
            time.sleep(0.02)
        self.check('owner cached shell output hidden from reader', self.run(reader, 'display-message', '-p', cache_fmt).strip() == '')
        writable = self.control(owner)
        restricted = self.control(reader)
        clients = self.run(owner, 'list-clients', '-F', '#{client_user}:#{client_name}')
        writer_name = next(line.split(':', 1)[1] for line in clients.splitlines() if line.startswith(str(owner) + ':'))
        for line in ['refresh-client -f !read-only', 'switch-client -r', 'run-shell ' + shell,
                     'refresh-client -t ' + writer_name + ' -f read-only',
                     'detach-client -t ' + writer_name, 'switch-client -c ' + writer_name + ' -n']:
            output = self.control_command(restricted, line)
            self.check('control rejects ' + line.split()[0] + ' ' + line.split()[1], 'client is read-only' in output)
        self.control_command(restricted, "display-message -p -c " + writer_name + " '#(" + shell + ")'")
        self.check('format target cannot substitute writer authority', not marker.exists())
        output = self.control_command(restricted, 'attach-session -t shared')
        self.check('reattach preserves ACL read-only', ' 1' in output)
        before = self.run(owner, 'display-message', '-p', '-t', 'shared', '#{pane_id}:#{window_panes}:#{session_windows}')
        wire = subprocess.Popen([sys.executable, str(Path(__file__).resolve()), '--wire-client', self.endpoint],
                                env=self.env(reader), preexec_fn=self.demote(reader),
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        stdout, stderr = wire.communicate(timeout=10)
        self.check('raw protocol read-only attach succeeds', wire.returncode == 0 and b'submitted' in stdout)
        assert wire.returncode == 0, stderr.decode('utf-8', 'replace')
        after = self.run(owner, 'display-message', '-p', '-t', 'shared', '#{pane_id}:#{window_panes}:#{session_windows}')
        capture = self.run(owner, 'capture-pane', '-p', '-t', 'shared', '-S', '-')
        self.check('raw keys paste mouse and reattach cannot mutate pane', before == after and 'FORBIDDEN' not in capture)
        self.run(owner, 'server-access', '-w', str(reader))
        self.control_command(restricted, 'new-window -d -t shared -n by-reader')
        self.check('owner can promote connected reader to writer', 'by-reader' in self.run(owner, 'list-windows', '-t', 'shared'))
        self.check('non-owner writer cannot change ACL', 'owner' in self.run(reader, 'server-access', '-a', str(self.outsider), expected=False))
        self.run(owner, 'server-access', '-r', str(reader))
        self.check('downgrade applies to existing connection', 'read-only' in self.control_command(restricted, 'new-window -d'))
        self.run(owner, 'server-access', '-d', str(reader))
        restricted.wait(timeout=5)
        self.check('revocation disconnects existing reader', restricted.returncode is not None)
        self.check('revoked UID cannot reconnect', bool(self.run(reader, 'ls', expected=False)))
        self.check('writer connection remains usable', ' 0' in self.control_command(writable, 'display-message -p WRITER_ALIVE'))
        return dict(cases=len(self.cases), passed=len(self.cases), checks=self.cases)

    def close(self):
        try:
            self.run(self.owner, 'kill-server', expected=None)
        except (OSError, subprocess.TimeoutExpired):
            pass
        for proc in self.children:
            if proc.poll() is None:
                proc.terminate()
                proc.wait(timeout=5)
        # Only remove the exact directory returned by mkdtemp for this run.
        assert self.base.parent == Path(tempfile.gettempdir()) and self.base.name.startswith('pt-security-')
        shutil.rmtree(str(self.base))


def main():
    if len(sys.argv) == 3 and sys.argv[1] == '--wire-client':
        wire_client(sys.argv[2])
        return
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('binary')
    args = parser.parse_args()
    if os.name != 'posix' or os.geteuid() != 0:
        parser.error('requires Unix root for real UID changes; does not create accounts')
    audit = Audit(args.binary)
    try:
        print(json.dumps(audit.execute(), indent=2))
    finally:
        audit.close()


if __name__ == '__main__':
    main()
