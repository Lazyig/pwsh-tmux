#!/bin/sh
# server-access between two real Unix users (run as root in a throwaway container).
# usage: multiuser.sh /path/to/pwsh-tmux
set -u
B="$1"
cp "$B" /usr/local/bin/pt && chmod 755 /usr/local/bin/pt
id alice >/dev/null 2>&1 || useradd -m alice
id bob >/dev/null 2>&1 || useradd -m bob
AUID=$(id -u alice)
SOCK=/tmp/pwsh-tmux-$AUID/default
as_alice() { su alice -s /bin/sh -c "PWSH_TMUX_SHELL=/bin/sh $*"; }
as_bob() { su bob -s /bin/sh -c "$*"; }
fail() { echo "FAIL: $*"; as_alice pt kill-server; exit 1; }

as_alice pt kill-server >/dev/null 2>&1
as_alice pt new-session -d -s shared || fail "alice new-session"
# bob can't even reach the socket while nothing is shared
as_bob pt -S $SOCK ls >/dev/null 2>&1 && fail "bob reached an unshared server"
# read-only access: bob can look, not change
as_alice pt server-access -ar bob || fail "server-access -a"
as_alice pt server-access -l | grep -q "^bob (R)" || fail "list shows bob (R)"
as_bob pt -S $SOCK ls | grep -q "^shared:" || fail "read-only bob can list"
as_bob pt -S $SOCK new-window -t shared 2>&1 | grep -q "read-only" || fail "read-only bob was allowed to change"
# write access
as_alice pt server-access -w bob || fail "server-access -w"
as_bob pt -S $SOCK new-window -t shared -n by-bob || fail "writer bob new-window"
as_alice pt lsw -t shared | grep -q by-bob || fail "bob's window visible to alice"
# bob may not change access
as_bob pt -S $SOCK server-access -a root 2>&1 | grep -q "owner" || fail "bob changed access"
# deny again
as_alice pt server-access -d bob || fail "server-access -d"
as_bob pt -S $SOCK ls 2>&1 | grep -q "not allowed\|Permission denied" || fail "denied bob still got in"
# a third user was never allowed
id carol >/dev/null 2>&1 || useradd -m carol
as_alice pt server-access -a bob
su carol -s /bin/sh -c "pt -S $SOCK ls" 2>&1 | grep -q "not allowed" || fail "carol got in"
as_alice pt kill-server
echo "multiuser: OK"
