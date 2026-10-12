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

# ---- workspaces ----
fail() { echo "FAIL $1" >&2; exit 1; }
mkdir -p "$OUI_HOME/workspaces" "$dir/proj"
cat >"$OUI_HOME/workspaces/proj.toml" <<EOF
name = "proj"
cwd = "$dir/proj"
[[tab]]
profile = "work"
layout = { split = "right", ratio = 0.6, panes = [
  { cmd = "echo PANE-A" },
  { split = "down", panes = [
    { cmd = "echo PANE-B" },
    { cmd = "echo PANE-C" },
  ] },
] }
[[tab]]
profile = "personal"
layout = { cmd = "echo TAB-TWO" }
EOF

printf '[[tab]]\nprofile = "nobody"\n' >"$dir/bad.toml"
if out=$("$bin" open "$dir/bad.toml" 2>&1); then fail "invalid workspace is rejected"; fi
echo "$out" | grep -q "bad.toml:2: profile 'nobody' not found" || fail "error names file and line: $out"
echo "ok   invalid workspace is rejected with file:line"

"$bin" workspace list | grep -qE '^proj +2 tab\(s\), 4 pane\(s\) +\[work, personal\]' || fail "workspace list"
echo "ok   workspace list"

t new-window "'$bin' open proj; sleep 60"
wait_for '1 work +2 personal' "workspace opens with two tabs"
wait_for '^PANE-A' "pane 1 runs its command"
wait_for '│PANE-B' "pane 2 runs its command"
wait_for '│PANE-C' "pane 3 runs its command"
wait_for '├─' "panes are split right, then down"
t send-keys C-b 2
wait_for '^TAB-TWO' "second tab runs its command in its own profile"
t send-keys 'echo MARK-$((6*7)) $OUI_PROFILE' Enter
wait_for '^MARK-42 personal' "second tab runs in profile 'personal'"
t send-keys C-b d
wait_for 'detached from session' "detach from workspace"

t new-window "'$bin' open proj; sleep 60"
wait_for '^MARK-42 personal' "opening again attaches to the running session"
[ "$("$bin" ls | grep -c '^proj:')" = 1 ] || fail "no duplicate session"
"$bin" ls | grep -q '^proj: 2 tab' || fail "session keeps its 2 tabs"
echo "ok   no duplicate session"

"$bin" workspace save proj saved >/dev/null
saved=$OUI_HOME/workspaces/saved.toml
grep -q '^name = "saved"' "$saved" || fail "saved name"
grep -q '^profile = "work"' "$saved" && grep -q '^profile = "personal"' "$saved" || fail "saved profiles"
grep -q 'split = "right", ratio = 0.6' "$saved" && grep -q 'split = "down"' "$saved" || fail "saved split tree: $(cat "$saved")"
grep -q "proj\"" "$saved" || fail "saved cwd: $(cat "$saved")"
if "$bin" workspace save proj saved 2>/dev/null; then fail "save refuses to overwrite"; fi
echo "ok   workspace save writes profiles, splits and cwd"

t send-keys C-b d
wait_for 'detached from session' "detach again"
"$bin" kill proj >/dev/null
t new-window "'$bin' open saved; sleep 60"
wait_for '1 work +2 personal' "saved workspace reopens with both tabs"
wait_for '├─' "saved workspace restores the split tree"
"$bin" ls | grep -q '^saved: 2 tab' || fail "saved session has 2 tabs"
echo "ok   workspace save round-trips"
t send-keys C-b d
wait_for 'detached from session' "detach from saved workspace"

# --- Custom config: prefix, a remapped key, theme, status segments, general.
"$bin" kill-server >/dev/null # the daemon reads the config at startup
"$bin" profile new cfgp --fresh --color magenta >/dev/null
printf "PS1='$ '\n" >"$OUI_HOME/profiles/cfgp/home/.bash_profile"
export BASH_SILENCE_DEPRECATION_WARNING=1
cat >"$OUI_HOME/config.toml" <<'TOML'
theme = "light"
[general]
default_profile = "cfgp"
shell = "/bin/bash"
[keys]
prefix = "C-a"
[keys.bindings]
v = "split-right"
"%" = "none"
"|" = "none"
[status]
left = ["mode", "profile", { text = " <cfg> " }, "tabs-count"]
right = ["hints"]
TOML
"$bin" config check >/dev/null

t kill-session -t t
t new -d -s c -x 120 -y 30 "'$bin' new -s cfg; sleep 60"
wait_for 'oUI +1 cfgp' "no -p: first tab uses general.default_profile"
wait_for '● cfgp │ <cfg> │ 1 tab ' "custom status segments"
wait_for '\^a c tab · v/" split' "hints show the remapped prefix and split key"
if ! t capture-pane -p -e | grep -q '48;2;221;221;221'; then
    echo "FAIL light theme status bar colour" >&2; t capture-pane -p -e | tail -1 | od -c | head >&2; exit 1
fi
echo "ok   light theme status bar colour"
wait_for '^\$' "shell prompt"
t send-keys 'echo SH=$0' Enter
wait_for '^SH=/bin/bash' "general.shell overrides \$SHELL"
t send-keys C-a v
wait_for '│\$' "remapped split key (prefix C-a v)"
t send-keys C-a b
wait_for 'toggle-panel: not available yet' "unimplemented action flashes"
t send-keys C-a d
wait_for 'detached from session' "detach with C-a d"

sed -i.bak 's/^default_profile = .*/default_profile = "ghost"/' "$OUI_HOME/config.toml"
t kill-session -t c
t new -d -s g -x 160 -y 30 "'$bin' new -s ghost; sleep 60"
wait_for 'oUI +1 system' "missing default_profile falls back to system"
wait_for "default_profile: profile 'ghost' not found" "and warns in the status bar"
t send-keys C-a d
wait_for 'detached from session' "detach"

echo "all e2e checks passed (temp dirs: $dir $sockdir)"
