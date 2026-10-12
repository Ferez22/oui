#!/usr/bin/env bash
# End-to-end smoke test: runs oUI inside a private tmux server with its own
# OUI_HOME and socket in a fresh temp dir (never touches ~/.oui or your tmux).
#
#   scripts/e2e.sh [name]        name keeps socket/home/tmux server unique
#
# Env: E2E_TMP (parent of the temp dir, default $TMPDIR), E2E_SOCK_TMP (parent
# of a second short temp dir for the socket, default /tmp; unix socket paths
# must stay under ~100 chars), E2E_SHELL (pane shell, default /bin/sh).
# Nothing is deleted; the temp dirs are printed at the end.
set -euo pipefail

name=${1:-e2e}
root=$(cd "$(dirname "$0")/.." && pwd)
bin=$root/target/debug/oui
dir=$(mktemp -d "${E2E_TMP:-${TMPDIR:-/tmp}}/oui-$name.XXXXXX")
export OUI_HOME=$dir/home
sockdir=$(mktemp -d "${E2E_SOCK_TMP:-/tmp}/oui.XXXXXX")
export OUI_SOCKET=$sockdir/s.sock
export SHELL=${E2E_SHELL:-/bin/sh}
tm=oui-$name-$$
if [ ${#OUI_SOCKET} -ge 100 ]; then
    echo "socket path too long (${#OUI_SOCKET}); set E2E_SOCK_TMP to a shorter dir" >&2
    exit 1
fi

t() { tmux -L "$tm" "$@"; }
screen() { t capture-pane -p; }
cleanup() {
    "$bin" kill-server >/dev/null 2>&1 || true
    t kill-server 2>/dev/null || true
}
trap cleanup EXIT

# wait_for <grep -E pattern> <what>
wait_for() {
    for _ in $(seq 50); do
        if screen | grep -qE -- "$1"; then
            echo "ok   $2"
            return
        fi
        sleep 0.1
    done
    echo "FAIL $2 (pattern: $1)" >&2
    screen >&2
    exit 1
}

(cd "$root" && cargo build -q)
"$bin" profile new work --color green >/dev/null
"$bin" profile new personal --fresh >/dev/null
# Quiet, predictable prompt in the fresh profile.
printf "PS1='$ '\n" | tee "$OUI_HOME/profiles/personal/home/.profile" >"$OUI_HOME/profiles/personal/home/.zshrc"

t new -d -s t -x 120 -y 30 "'$bin' new -s demo -p personal; sleep 60"
wait_for 'oUI +1 personal' "session starts in profile 'personal'"
wait_for '^\$' "shell prompt"

t send-keys 'echo LEFT-$((6*7))' Enter
wait_for '^LEFT-42' "command runs in first pane"

t send-keys C-b %
wait_for '│\$' "split right shows a border and a new shell"
t send-keys 'echo RIGHT-$((6*7))' Enter
wait_for '│RIGHT-42' "typing goes to the focused (right) pane"

t send-keys C-b '"'
wait_for '├─' "split down draws a junction"
t send-keys 'echo DOWN-$((6*7))' Enter
wait_for '│DOWN-42' "command runs in bottom-right pane"

t send-keys C-b Left
t send-keys 'echo BACK-$((6*7))' Enter
wait_for '^BACK-42' "focus moves left"

t send-keys C-b z
wait_for ' ZOOM ' "zoom shows in status bar"
t send-keys C-b z
wait_for '├─' "unzoom restores the layout"

t send-keys C-b l C-b j C-b x
for _ in $(seq 50); do screen | grep -q '├' || break; sleep 0.1; done
if screen | grep -q '├'; then echo "FAIL closing a pane collapses the layout" >&2; screen >&2; exit 1; fi
echo "ok   closing a pane collapses the layout"

t send-keys C-b d
wait_for 'detached from session' "detach"
if ! "$bin" ls | grep -q '^demo: 1 tab'; then echo "FAIL session survives detach" >&2; exit 1; fi
echo "ok   session survives detach"

echo "all e2e checks passed (temp dirs: $dir $sockdir)"
