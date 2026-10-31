"""Keep UI-reference CLI processes inside one WSL session.

Hex-framed requests avoid Windows argv quoting and WSL 1 SessionLeader failures
when a menu/popup CLI deliberately remains alive until a choice is made.
"""
import os
import subprocess
import sys

env = os.environ.copy()
for key in ('TMUX', 'TMUX_PANE', 'PWSH_TMUX', 'PWSH_TMUX_PANE', 'PWSH_TMUX_SOCKET', 'PWSH_TMUX_SOCKET_PATH', 'COLORTERM'):
    env.pop(key, None)
env.update(TERM='xterm-256color', LC_ALL='C.UTF-8')
children = []
try:
    for line in sys.stdin.buffer:
        operation, _, encoded = line.strip().partition(b' ')
        args = bytes.fromhex(encoded.decode()).decode('utf-8').split('\0') if encoded else []
        code, stdout, stderr = 0, b'', b''
        try:
            if operation == b'RUN':
                result = subprocess.run(args, env=env, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=10)
                code, stdout, stderr = result.returncode, result.stdout, result.stderr
            elif operation == b'START':
                children.append(subprocess.Popen(args, env=env, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL))
            elif operation == b'PENDING':
                stdout = str(sum(p.poll() is None for p in children)).encode()
            elif operation == b'CLOSE':
                break
            else:
                raise ValueError('unknown operation')
        except Exception as error:
            code, stderr = 1, str(error).encode()
        sys.stdout.buffer.write(('%d %s %s\n' % (code, stdout.hex(), stderr.hex())).encode())
        sys.stdout.buffer.flush()
finally:
    for child in children:
        if child.poll() is None:
            child.terminate()
        child.wait()
