"""Compare CLI behaviour with a real tmux using isolated servers.

Windows example (requires WSL Ubuntu and Python 3):
  python tests/tmux_compat.py --candidate target/debug/pwsh-tmux.exe \
    --reference C:/Windows/System32/wsl.exe -d Ubuntu --exec /usr/bin/tmux
On Unix use --reference tmux. Both programs must already be built/installed.
"""

import argparse
import json
import os
import subprocess
import time
import uuid


class Server:
    def __init__(self, command, candidate=False):
        self.command = command
        self.candidate = candidate
        self.socket = "compat-" + uuid.uuid4().hex
        self.config = "NUL" if candidate and os.name == "nt" else "/dev/null"

    def run(self, *args, check=True):
        env = os.environ.copy()
        for key in ("TMUX", "TMUX_PANE", "PWSH_TMUX", "PWSH_TMUX_PANE", "PWSH_TMUX_SOCKET"):
            env.pop(key, None)
        result = subprocess.run(self.command + ["-L", self.socket, "-f", self.config] + list(args),
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE, universal_newlines=True, encoding="utf-8", errors="replace",
                                env=env, timeout=30)
        if check and result.returncode:
            raise RuntimeError(f"{args}: {result.stderr}{result.stdout}")
        return result.stdout.rstrip("\r\n")

    def fmt(self, template, target="compat:0"):
        return self.run("display-message", "-p", "-t", target, template)

    def panes(self):
        return self.run("list-panes", "-t", "compat:0", "-F",
                        "#{pane_index}:#{pane_left},#{pane_top},#{pane_width},#{pane_height}")

    def collect(self):
        self.run("new-session", "-d", "-s", "compat", "-x", "121", "-y", "41")
        self.run("set", "-g", "base-index", "0")
        self.run("set", "-g", "pane-base-index", "0")
        for _ in range(2):
            self.run("split-window", "-d", "-t", "compat:0.0")
        out = {}
        self.run("next-layout", "-t", "compat:0")
        out["first next-layout"] = self.panes()
        for name in ("even-horizontal", "even-vertical", "main-horizontal", "main-horizontal-mirrored",
                     "main-vertical", "main-vertical-mirrored", "tiled"):
            self.run("select-layout", "-t", "compat:0", name)
            out[name] = self.panes()
        # A preset must keep pane indices stable even when the main pane moves.
        self.run("select-layout", "-t", "compat:0", "main-vertical-mirrored")
        self.run("select-layout", "-t", "compat:0", "even-horizontal")
        self.run("select-layout", "-o", "-t", "compat:0")
        out["restore mirrored geometry"] = self.panes()
        self.run("select-layout", "-t", "compat:0", "main-vertical-mirrored")
        self.run("split-window", "-d", "-h", "-t", "compat:0.0")
        out["split mirrored main pane"] = self.panes()
        self.run("kill-pane", "-t", "compat:0.1")
        self.run("select-layout", "-t", "compat:0", "even-horizontal")
        self.run("select-layout", "-t", "compat:0", "even-vertical")
        self.run("select-layout", "-o", "-t", "compat:0")
        out["restore layout"] = self.panes()
        self.run("select-layout", "-o", "-t", "compat:0")
        out["restore layout twice"] = self.panes()
        self.run("select-layout", "-t", "compat:0", "even-horiz")
        out["layout prefix"] = self.panes()

        self.run("set", "-g", "@sample", "abc-123 abc-456")
        self.run("set", "-g", "@pattern", "^[[:alpha:]]+-[[:digit:]]{3}$")
        self.run("set", "-g", "@unicode", "\uac00\ub098")
        formats = [
            r"#{s/([a-z]+)-([0-9]+)/\2_\1/:@sample}",
            r"#{s/a/A/;s/b/B/:@sample}",
            r"#{m/r:^(abc|def)-[0-9]{3}$,abc-123}",
            r"#{m/r:[[:digit:]]{3},abc-123}",
            r"#{m/r:#{@pattern},abc-123}",
            r"#{s/(a|ab)/X/:#{l:ab}}",
            r"#{s/a*/X/:#{l:ab}}",
            r"#{s/a/\0/:#{l:aba}}",
            r"#{s/a/&/:#{l:aba}}",
            r"#{s/a/$1/:#{l:aba}}",
            r"#{s/a/\q/:#{l:aba}}",
            r"#{m/r:\d,1}",
            r"#{m/r:\d,d}",
            r"#{m/r:\w,a}",
            r"#{m/r:\s, }",
            r"#{m/r:\ba,a}",
            r"#{m/r:(?=a),a}",
            r"#{e|m|:7,2}",
            r"#{e|/|f:7,2}",
            r"#{s/./X/:@unicode}",
            r"#{n:@unicode}",
            r"#{?missing,yes,a,b}",
            r"#{?missing,yes,session_name,other,last}",
            r"#{?session_name,yes}",
            r"#{S:#{session_name};}",
            r"#{W:#{window_index},[#{window_index}]}",
            r"#{P:#{pane_index},[#{pane_index}]}",
            r"#{S:#{W:#{P:#{pane_index}}}}",
        ]
        for template in formats:
            out[template] = self.fmt(template)

        self.run("new-session", "-d", "-s", "aaa")
        self.run("new-window", "-d", "-t", "compat", "-n", "aaa")
        self.run("rename-window", "-t", "compat:0", "zzz")
        for template in ("#{S:#S;}", "#{S/n:#S;}", "#{S/r:#S;}", "#{W:#W;}",
                         "#{W/n:#W;}", "#{W/r:#W;}", "#{P/r:#P;}"):
            out[template] = self.fmt(template)

        self.run("kill-pane", "-a", "-t", "compat:0.0")
        self.run("split-window", "-d", "-h", "-l", "30", "-t", "compat:0.0")
        self.run("split-window", "-d", "-v", "-l", "10", "-t", "compat:0.1")
        self.run("select-layout", "-E", "-t", "compat:0.1")
        out["spread target siblings"] = self.panes()
        self.run("select-layout", "-E", "-t", "compat:0.1")
        out["spread ancestor when siblings even"] = self.panes()

        # Search a known visible screen, independent of shell prompt contents.
        if self.candidate and os.name == "nt":
            command = "[Console]::Write([char]27 + '[2J' + [char]27 + '[Hfirst(a [b] (c))' + [char]13 + [char]10 + 'needle-123'); Start-Sleep -Seconds 600"
        else:
            command = "printf '\033[2J\033[Hfirst(a [b] (c))\r\nneedle-123'; sleep 600"
        self.run("respawn-pane", "-k", "-t", "compat:0.0", command)
        deadline = time.monotonic() + 15
        while "needle-123" not in self.run("capture-pane", "-p", "-t", "compat:0.0"):
            if time.monotonic() > deadline:
                raise RuntimeError("fixture output did not appear")
            time.sleep(0.1)
        for template in ("#{C:needle}", "#{C:need*123}", "#{C/i:NEEDLE}",
                         "#{C/r:needle-[0-9]+}", "#{C:not-here}"):
            out[template] = self.fmt(template, "compat:0.0")
        self.run("set-window-option", "-t", "compat:0", "mode-keys", "vi")
        self.run("copy-mode", "-t", "compat:0.0")
        self.run("send-keys", "-X", "-t", "compat:0.0", "history-bottom")
        self.run("send-keys", "-X", "-t", "compat:0.0", "search-backward", "first")
        self.run("send-keys", "-X", "-t", "compat:0.0", "start-of-line")
        for name in ("next-matching-bracket", "next-matching-bracket", "previous-matching-bracket"):
            self.run("send-keys", "-X", "-t", "compat:0.0", name)
            key = "bracket " + name + " " + str(len(out))
            out[key] = self.fmt("#{copy_cursor_x},#{copy_cursor_y}", "compat:0.0")
        for pattern in ("needle-[[:digit:]]+", "first.*", "[", "(needle|first)"):
            self.run("send-keys", "-X", "-t", "compat:0.0", "search-forward", pattern)
            out["copy search " + pattern] = self.fmt("#{copy_cursor_x},#{copy_cursor_y}", "compat:0.0")
        return out


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--candidate", required=True)
    parser.add_argument("--reference", nargs=argparse.REMAINDER, required=True)
    args = parser.parse_args()
    if not args.reference:
        parser.error("--reference requires a command")
    candidate = Server([os.path.abspath(args.candidate)], candidate=True)
    reference = Server(args.reference)
    try:
        expected = reference.collect()
        actual = candidate.collect()
        failures = {key: {"tmux": expected[key], "candidate": actual.get(key)}
                    for key in expected if actual.get(key) != expected[key]}
        print(json.dumps({"cases": len(expected), "passed": len(expected) - len(failures),
                          "differences": failures}, indent=2, ensure_ascii=True))
        return bool(failures)
    finally:
        for server in (candidate, reference):
            server.run("kill-server", check=False)


if __name__ == "__main__":
    raise SystemExit(main())
