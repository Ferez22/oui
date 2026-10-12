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

# --- Scrollback, copy mode, mouse, clipboard (default keys, mouse on).
"$bin" kill-server >/dev/null
printf '[general]\ndefault_profile = "personal"\nscrollback = 2000\n' >"$OUI_HOME/config.toml"
clip=$dir/clipboard.txt
# raw <bytes>: inject raw bytes (mouse reports) as if typed in the host terminal.
raw() { t send-keys -H $(printf '%b' "$1" | od -An -tx1); }
# clip_is <text> <what>: the clipboard test hook file holds exactly <text>.
clip_is() {
    for _ in $(seq 50); do
        if [ "$(cat "$clip" 2>/dev/null)" = "$1" ]; then echo "ok   $2"; return; fi
        sleep 0.1
    done
    echo "FAIL $2 (clipboard: $(cat "$clip" 2>/dev/null))" >&2; exit 1
}
t kill-session -t g
t new -d -s m -x 120 -y 30 "OUI_CLIPBOARD_FILE='$clip' '$bin' new -s copy; sleep 60"
t set-option -g set-clipboard on # OSC 52 from oUI lands in this server's paste buffer
wait_for '^\$' "shell prompt"
t send-keys 'seq 1 500' Enter
wait_for '^500$' "seq output"
t send-keys C-b [
wait_for ' COPY ' "copy mode shows in the status bar"
wait_for '\[0/[0-9]+\]' "position indicator at the bottom"
t send-keys g
wait_for '^1$' "g scrolls to the top of the history"
wait_for '\[4[0-9][0-9]/4[0-9][0-9]\]' "indicator shows the scroll offset"
t send-keys / 2 3 4 Enter
wait_for '^234$' "search finds a line"
t send-keys V j y
clip_is "$(printf '234\n235')" "line selection yanked to the clipboard"
if t show-buffer 2>/dev/null | grep -q '^235$'; then echo "ok   OSC 52 reached the host terminal"; else
    echo "FAIL OSC 52 reached the host terminal" >&2; exit 1; fi
for _ in $(seq 50); do screen | grep -q ' COPY ' || break; sleep 0.1; done
wait_for '^500$' "yank leaves copy mode at the bottom"
t send-keys C-b [ ? 1 2 0 Enter v e y
clip_is "120" "backward search + char selection"

t send-keys 'clear' Enter
wait_for '^\$$' "clear"
raw '\e[<64;10;10M\e[<64;10;10M'
wait_for '\[6/[0-9]+\]' "mouse wheel scrolls into history"
raw '\e[<65;10;10M\e[<65;10;10M'
for _ in $(seq 50); do screen | grep -q ' COPY ' || break; sleep 0.1; done
if screen | grep -q ' COPY '; then echo "FAIL wheel back to the bottom leaves scroll view" >&2; screen >&2; exit 1; fi
echo "ok   wheel back to the bottom leaves scroll view"

t send-keys 'clear; echo DRAGME' Enter
wait_for '^DRAGME' "text to drag over"
raw '\e[<0;1;2M\e[<32;4;2M\e[<32;6;2M\e[<0;6;2m'
clip_is "DRAGME" "mouse drag selects and copies"

t send-keys C-b %
wait_for '│\$' "split for click focus"
raw '\e[<0;5;5M\e[<0;5;5m'
t send-keys 'echo CLICK-$((6*7))' Enter
wait_for '^CLICK-42' "click focuses the left pane"
raw '\e[<0;100;5M\e[<0;100;5m'
t send-keys 'printf "\\033[?1000h\\033[?1006h"; cat -v' Enter
sleep 0.3
raw '\e[<0;70;6M\e[<0;70;6m'
wait_for '│\^\[\[<0;[0-9]+;5M' "mouse reports go to an app that asked for them (pane-relative)"
t send-keys C-c

t send-keys C-b c Enter
wait_for 'oUI +1 personal +2 personal' "second tab"
raw '\e[<0;8;1M\e[<0;8;1m'
wait_for '^CLICK-42' "click on a tab selects it"
t send-keys C-b d
wait_for 'detached from session' "detach"

# Reattached clients get the history from the daemon; apps' OSC 52 is forwarded.
t kill-session -t m
t new -d -s r -x 120 -y 30 "OUI_CLIPBOARD_FILE='$clip' '$bin' attach copy; sleep 60"
wait_for '^CLICK-42' "reattach"
raw '\e[<0;5;5M\e[<0;5;5m'
t send-keys C-b [ g
wait_for '^1 +│' "history from the daemon after reattach"
t send-keys q
t send-keys "printf '\\033]52;c;%s\\007' \$(printf APPCLIP | base64)" Enter
clip_is "APPCLIP" "OSC 52 from an app in a pane reaches the clipboard"
t send-keys C-b d
wait_for 'detached from session' "detach"

echo "all e2e checks passed (temp dirs: $dir $sockdir)"
