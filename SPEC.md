# oUI — Specification (draft v0.1)

oUI is an open-source terminal workspace for macOS and Linux, written in Rust.
It does two things other terminals don't do together:

1. **Profiles with isolated logins.** Each profile has its own login for every CLI tool. Run `claude` → `/login`, `railway login`, `gh auth login`, … inside a profile and the login belongs to that profile only. Two Claude subscriptions, two Railway accounts, side by side, never overwriting each other.
2. **tmux-style sessions** with a modern UI: tabs, splits, persistence, status bar, collapsible right panel.

## Goals
- Works out of the box. No per-tool setup: user just runs the tool's normal login command.
- Login once per profile; oUI remembers it. Logging in again replaces only that profile's login.
- Clean, custom design (user-provided styling). Tabs always show which profile they run.
- Open source from day 1.

## Non-goals (v1)
- Remote attach (planned v3).
- Own GPU renderer (planned GUI client).
- Windows.

## Architecture

```
oui-daemon (Rust, tokio)
  - owns PTYs, profiles, sessions, layouts, persistence
  - unix socket API
     ├── oui (TUI client, ratatui)   ← v1, runs inside any terminal
     └── oui-gui (later)             ← custom design, GPU, fonts
```

Crates: `portable-pty` (spawn shells), `alacritty_terminal` or `vt100` (terminal emulation inside panes), `ratatui` + `crossterm` (TUI), `tokio`, `serde`/`toml` (config).

The daemon keeps shells alive when clients disconnect (tmux model). Clients are thin.

- **Protocol:** every frame starts with a magic and a protocol version (`oui --version`); client and daemon refuse other versions instead of misreading them, and the client tells the user to run `oui kill-server`, which also stops older daemons.
- **Robustness:** a pane's input is written by its own thread through a bounded queue (4 MiB; a pane that stops reading gets further input dropped with a `pane is not reading input` message), so no pane can block other sessions. Panes are at least 2 columns × 1 row (splits below that are refused); emulator panics are contained and reset only that pane's screen.

## Profiles & login isolation

Layout:
```
~/.oui/                 # 0700, must belong to the user ($OUI_HOME too; refused otherwise)
  config.toml           # 0600, like daemon.log, workspaces/*.toml, profile.toml
  profiles/<name>/
    profile.toml        # name, color, shared, extra_shared, keychain, protected
    keychain.pw         # macOS, keychain = "file" only (0600)
    bin/ssh             # only when home/.ssh/config exists: ssh wrapper (see adapters)
    home/               # the profile's HOME
      .config/ .local/share/ .local/state/ .cache/ .claude/ ...
      Library/Keychains/oui.keychain-db    # macOS: the profile keychain (0600)
      Library/Keychains/login.keychain-db  # symlink to it (makes it the default)
```
Every folder oUI creates there is 0700 (root, `profiles/`, the profile, its HOME, `Library`, `Library/Keychains`): a real macOS `~/Library` is 0700, and the profile HOME stands in for it. Each spawn tightens these folders (and `profile.toml`, the keychain file) if an older version left them more open; symlinks are never followed.

Every process in a profile gets:

| Variable | Value | Why |
|---|---|---|
| `HOME` | `profiles/<name>/home` | Tools storing logins in `~/.x` (railway, vercel, npm, aws, docker) isolate automatically |
| `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `XDG_STATE_HOME`, `XDG_CACHE_HOME` | inside profile home | Tools using XDG dirs (gh, stripe, many Go/Rust CLIs) |
| `CLAUDE_CONFIG_DIR` | `home/.claude` | Claude Code adapter |
| `GH_CONFIG_DIR` | `home/.config/gh` | gh adapter (explicit) |
| `OUI_PROFILE`, `OUI_PROFILE_COLOR`, `OUI_REAL_HOME` | — | Prompt/Starship integration, tooling |
| `TERM` | `xterm-256color` | Works on every server (fixes nano/vim over ssh) |
| `PATH`, `GIT_SSH_COMMAND`, `OUI_PROFILE_BIN` | `profiles/<name>/bin` first; its `ssh` | Only when the profile has `home/.ssh/config` (ssh adapter). Other profiles' `bin` folders are always removed from `PATH`. |

Removed from the environment so they can't override profile logins (`profile::CREDENTIAL_VARS`, list grows with adapters): `ANTHROPIC_API_KEY`, `CLAUDE_CODE_OAUTH_TOKEN`, `RAILWAY_TOKEN`, `RAILWAY_API_TOKEN`, `GH_TOKEN`, `GITHUB_TOKEN`, `VERCEL_TOKEN`, `NPM_TOKEN`, `FLY_ACCESS_TOKEN`, `FLY_API_TOKEN`, `SUPABASE_ACCESS_TOKEN`, `STRIPE_API_KEY`, `NETLIFY_AUTH_TOKEN`, `AWS_*` credentials/profile/config paths, `DOCKER_CONFIG`, `GIT_CONFIG_GLOBAL`, `CARGO_HOME`, `CARGO_REGISTRY_TOKEN`, `KUBECONFIG`, `GOOGLE_APPLICATION_CREDENTIALS`, `CLOUDSDK_CONFIG`, `AZURE_CONFIG_DIR`; plus every `CARGO_REGISTRIES_*_TOKEN` and every `npm_config_*` (any case) naming an auth token, `userconfig` or `globalconfig`. Shell rc files run after this and can re-export variables (see Shared).

### Shared vs fresh
- **Shared** (default): non-credential dotfiles and toolchains are symlinked from the real HOME (`.zshrc`, `.oh-my-zsh`, `.nvm`, `.rustup`, `starship.toml`, …). Allowlist only — credential locations are never linked.
  - Folders mixing toolchain and credentials (`SHARED_PARTS`) get a real per-profile folder (0700) with links to their credential-free parts only: `.cargo/{bin,env,env.fish,env.nu}`. `cargo login` therefore writes the profile's own `.cargo/credentials.toml`. Cargo's `config.toml` (can hold tokens) and its `registry/`/`git/` caches (locked through a file in the cargo home, so not safely shareable between cargo homes) stay per profile.
  - Migration: profiles from older versions have `home/.cargo` → real `~/.cargo`. At the next spawn that symlink (only one pointing at the real `~/.cargo`) is removed, never followed, and the folder is rebuilt; the real `~/.cargo` is untouched. A login made in such a profile before the upgrade stays in the real `~/.cargo`.
  - Linked files are still the user's: tokens exported from shared rc files, and fish universal variables (`set -Ux`, stored in the linked `.config/fish/fish_variables`), reach every shared profile.
  - `extra_shared` (profile.toml): extra paths relative to HOME. Absolute paths, `.`/`..` components and anything at or under `Library/Keychains` (case-insensitive) are refused.
- **Fresh**: empty home.
- Toggle anytime (`oui profile share <name> true|false`). Unsharing removes only oUI's symlinks; profile logins are kept. UI warns before toggling.

### Adapters (tools that don't follow HOME/XDG)
| Case | Tools | Plan |
|---|---|---|
| macOS Keychain | Claude Code, gh (keyring), git-credential-osxkeychain, anything using the default keychain | **Per-profile keychain** (below) — no per-tool code. |
| Linux Secret Service | some CLIs (gh, supabase keyring mode) | Out of scope for v1 (most Linux CLIs fall back to files, which HOME/XDG isolate). |
| Ignores `HOME` (uses passwd entry) | `ssh` | When `home/.ssh/config` exists, each spawn writes `profiles/<name>/bin/ssh` (0700 folder): `exec <real ssh> -F <profile>/home/.ssh/config "$@"`, the real ssh being the first non-wrapper `ssh` on PATH (else `/usr/bin/ssh`). That folder goes first on `PATH`, `GIT_SSH_COMMAND` points at the wrapper (git always uses it) and `OUI_PROFILE_BIN` names the folder. macOS login shells run `path_helper`, which moves system dirs ahead of it; the README gives a one-line `.zshrc` fix for typing `ssh`. No config: no wrapper (removed if present), ssh untouched. The ssh agent and `~/.ssh/known_hosts` stay shared. |
| git identity | git | `~/.gitconfig` in the profile HOME (not linked); `GIT_CONFIG_GLOBAL` is removed so it can't point back at the real one |

#### Per-profile macOS keychain
macOS resolves a process's default keychain through `$HOME` (`~/Library/Keychains/login.keychain-db`). Each profile home therefore gets `Library/Keychains/oui.keychain-db` plus a `login.keychain-db` symlink to it, and every tool in the profile that stores secrets in "the keychain" stores them there — isolated from other profiles and from the real login keychain, with no per-tool code. (The file isn't itself named `login.keychain-db`: securityd treats a keychain with that name as the account's login keychain and won't unlock it with its own password.)

- **Password**: 32 random bytes (hex), generated by oUI. Never on a command line (creation goes through `security -i` stdin, unlock through the Security framework), never logged; zeroized in memory where practical. A stored password is used only if it is exactly 64 lowercase hex digits. A new keychain (no keychain file yet) always gets a freshly generated password, replacing whatever the store held. `keychain.pw` is opened with `O_NOFOLLOW` (read and write) and must be a regular file owned by the user. An existing `login.keychain-db` symlink is trusted only if it points to `oui.keychain-db`.
- **Mode** (`keychain` in `profile.toml`, chosen at `oui profile new` — interactive prompt on a TTY, or `--keychain secure|file`):
  - `secure` (default): password stored as a generic password item (service `oUI profile keychain`, account = profile name) in the **real** login keychain, opened by explicit path from the real HOME. Encrypted at rest; macOS may ask once to let `oui` read the item ("Always Allow"; again after `oui` is rebuilt/upgraded, since unsigned binaries change identity).
  - `file`: password in `profiles/<name>/keychain.pw` (0600). No prompts; protection equals plaintext token files.
- **Spawn**: every process oUI starts in a profile (`daemon` panes, `oui shell`, `oui run`) first ensures the keychain exists, is unlocked and has no auto-lock / lock-on-sleep. Already unlocked = one status call. Never blocks the shell: panes wait at most 3 s (then the unlock finishes in the background, e.g. while macOS shows its prompt), `oui shell`/`run` up to 60 s; failures show as a yellow `oUI ▸` line in the pane / a warning on stderr.
- **Existing profiles** (no `keychain` field) are `secure`; their keychain is created at the first shell.
- `oui profile keychain <name> secure|file` moves the password between stores (contents kept). `oui profile rm <name>` (typed confirmation) deletes the profile folder and the login-keychain item.
- Creation runs `security create-keychain` with `HOME` = the profile home: creating a keychain adds it to the *creating HOME's* search list, and it must never join the real one (real-environment lookups would then see profile secrets).
- Accounts: Claude Code counts as logged in when the profile keychain (if unlocked) has an item whose service starts with `Claude Code` — attributes only, no secret read, no UI.
- Linux: no-op.

An **isolation test suite** runs each supported tool in two profiles and checks logins don't leak. Contributors add tools to it.

### Guardrails
- Profile color on tab, pane border, status bar.
- `protected = true` profiles ask before risky commands (`railway up`, `vercel --prod`, `git push --force`, …). (v2)

## Sessions (tmux-style)
- Hierarchy: **Session → Tabs → Panes**. Each tab runs one profile; tab label shows profile name + color.
- Prefix key (default `Ctrl-b`), configurable. Default keys after the prefix: `c` new tab, `n`/`p`/`1-9` switch tab, `%`/`|` split right, `"`/`-` split down, arrows/`hjkl` focus, `HJKL` resize, `z` zoom, `d`/`x` close pane (on the last pane of the last tab: "Close oUI?" with `y` end session / `d` detach / `n` cancel), `&` close tab, `D` detach, `b` right panel, `Tab` focus the panel, `[` copy mode, `s` session picker, `,` rename tab; prefix twice sends it to the pane.
- Keys after the prefix are matched as whole keys (a full CSI/SS3 sequence, Alt+key, a UTF-8 character). An explicit binding wins; otherwise an arrow with modifiers (xterm `ESC [ 1 ; m X`, Alt as `ESC ESC [ X`, rxvt forms) acts like the plain arrow (focus) with Ctrl/Alt — users often still hold Ctrl after `Ctrl-b` — and resizes with Shift (`S-`, `C-S-`, `M-S-`). Anything unbound is swallowed entirely (never leaked to the pane as `[1;5D`) and flashed as `unknown key: ^b …`; `Esc` cancels quietly. A sequence split across reads is reassembled.
- **Repeat** (like tmux `bind -r`): after a focus/resize action, for 600 ms arrows (any modifiers) and `H J K L` act again without the prefix, each one extending the window; the status bar shows `REPEAT`. Lowercase `hjkl` never repeat (typing right after `prefix l` stays the shell's); any other key ends the window and goes to the pane.
- **Confirmations** (yes/no box, `y` confirms, anything else = No): detach (`Detach from oUI? Your shells keep running…`); closing the last pane of the last tab, the last tab, or killing the current session from the picker when it's the only one (`Close oUI? This ends session '<name>' and its shells.`); closing a pane/tab where a program other than the shell runs in the foreground (the daemon compares the PTY's foreground process group with the shell's pid: `Close this pane? 'vim' is still running in it.`). Other closes happen at once. `exit` in the last shell ends the session without a prompt; the client prints a goodbye (and, after detach, how to reattach).
- Detach (`prefix d`) / attach (`oui attach <session>`). Shells survive client exit.
- Session manager UI: list, rename, kill, switch.
- **Workspaces** (`~/.oui/workspaces/<name>.toml`, see [docs/workspaces.md](docs/workspaces.md)): layout files declaring tabs, profile, cwd, a nested split tree, and startup commands (typed into the pane's shell). `oui open <name|path>` creates the session or attaches if running; `oui workspace list | new | save`.
  ```toml
  [[tab]]
  profile = "work"
  cwd = "~/code/api"
  layout = { split = "right", panes = [
    { cmd = "claude" },
    { split = "down", panes = [{ cmd = "railway logs" }, { cmd = "npm run dev" }] },
  ] }
  ```
- Resurrection after reboot (relaunch commands in saved cwd): v2.

## UI
- **Tab bar** (top, full width in `tab_bar_bg`): an ` oUI ` badge (accent), each tab as a block — number, optional name (`prefix ,`; shown as `1 name · profile`), profile name in the profile colour on `tab_inactive_bg`; the active tab is bold on its profile colour — then a ` + ` button (click: new-tab picker) and `session <name>` flush right (click: session picker; dropped first on narrow terminals). Clicking a tab selects it. Too many tabs: the visible range scrolls to keep the active tab shown, `‹N` / `N›` count hidden tabs and select the nearest one when clicked.
- **Status bar** (bottom): profile, session, mode, key hints, clock. Segments configurable. The mode badge shows `PREFIX`, `PANEL`, `SESSIONS` or `RENAME`.
- **Right panel** (collapsible, `prefix b`; initial state `panel.visible`, width `panel.width`): the pane area shrinks by its width and the client reports the smaller size, so shells reflow. Widgets in `panel.widgets` order:
  - **Accounts** for the active tab's profile: `●` logged in (with the account when known), `○` logged out, `?` can't tell (login may sit in a keyring — never shown as logged out; Claude Code in a profile's own macOS keychain is detected by item attributes). Tools that aren't installed and have no login are hidden behind a dim `+N not installed` line. Detection is offline/filesystem-only and runs off the draw path; it refreshes when the panel opens, when the active profile changes, and every 5 s while visible.
  - **Sessions** with tab counts, current marked `▸`.
  - **Profiles** with colour dot and shared/fresh.
  - `prefix Tab` focuses the panel (`j`/`k`/arrows, `Esc`/`q` back). `Enter`: on an account, opens a new tab in the same profile that runs the tool's login command (Claude Code: runs `claude` and tells you to type `/login`; git: shows the `git config` command instead of running it); on a session, switches to it; on a profile, opens a tab in it.
  - v2: plugin widgets.
- **Session picker** (`prefix s`): list with tab counts; `Enter` switch, `n` new session (first tab in the default profile), `x` kill with y/n confirmation (killing the current session moves you to another one first).
- The prefix closes the new-tab picker, the session picker, the rename prompt or panel focus, then works as usual (as it leaves copy mode); `prefix Tab` therefore toggles panel focus. The status bar mode badge shows `PREFIX`, `REPEAT`, `COPY`, `PANEL`, `SESSIONS`, `RENAME`, `CONFIRM` or `SETUP` (plus `ZOOM`). Key hints put `d close pane` in bold.
- New-tab profile picker, session picker and prompts use the theme's panel colours (`panel_bg`, `panel_fg`, `accent`).
- **Starship**: oUI ships a `custom.oui` module showing the active profile.
- Scrollback search, copy mode with vi keys (see below).
- Styling from a theme file; user provides the design.

## Copy mode, mouse, clipboard
- **Scrollback**: each pane's emulator in the daemon keeps `general.scrollback` lines. Clients mirror pane screens and keep their own scrollback as output arrives; entering copy mode (or wheel-scrolling) on a pane fetches the daemon's full history once (`History` request → screen + history replayed into the client's emulator). Keys typed before it arrives are replayed after. The view is anchored: new output grows the history without moving what you're looking at; `[offset/total]` is drawn at the pane's top right.
- **Copy mode** (`prefix [`): vi keys — `hjkl`/arrows, `w b e`, `0 ^ $`, `g G`, `H M L`, `Ctrl-u/d`, `PgUp/PgDn` (also `Ctrl-b/f` when not the prefix), `/` `?` search (lowercase query = case-insensitive, wraps), `n N`, `v` char / `V` line selection, `y`/`Enter` copy + exit, `q`/`Esc` exit. Wrapped lines are joined when copied. Status bar mode badge shows `COPY` and the search prompt. The prefix leaves copy mode and then acts normally. A resize of the pane (new snapshot) leaves copy mode.
- **Mouse** (`general.mouse`, default on): the client enables SGR mouse reporting (1000/1002/1006) on the host terminal. Wheel over a pane enters a scroll view (copy mode, left automatically when scrolled back to the bottom); click focuses a pane, click on a tab selects it; drag selects (scrolling past the pane edge) and copies on release. A full-screen app without mouse support gets arrow keys for the wheel. If a pane's app enabled mouse reporting itself, events go to it instead, re-encoded relative to the pane in the mode (press / press-release / button-motion / any-motion) and encoding (X10, UTF-8, SGR) it asked for; drags stay with the pane they started in. Shift+drag is left to the host terminal (most terminals bypass mouse reporting with Shift). Hover motion (mode 1003) is not requested from the host, so any-motion apps only see motion while a button is held. Mouse events are ignored while an overlay has the keyboard (new-tab picker, session picker, rename prompt, focused panel) and over the right panel; they never reach the panes underneath.
- **Clipboard**: the user's copies (copy mode, mouse drag) write OSC 52 to the host terminal (works over ssh; skipped above 100 KB of text) and pipe into `pbcopy` (macOS) or `wl-copy`/`xclip` (Linux) when present. OSC 52 emitted by apps in panes (≤ ~100 KB) is caught by the client's own emulator for the pane, so the client knows where it came from: only writes from the focused pane of the active tab count, at most one per 100 ms (later ones in a burst are held back, the newest wins). `clipboard.apps` decides what happens: `osc52` (default) re-emits it to the host terminal as OSC 52 only, so the host terminal's clipboard permission applies (like tmux `set-clipboard on`); `off` ignores it; `system` also pipes it into the platform tool (opt-in: any program or file printed in the pane can then replace the clipboard even where the host terminal refuses OSC 52). The daemon's unattributed `ServerMsg::Clipboard` is ignored. Clipboard *reads* (OSC 52 `?`) are never answered. `OUI_CLIPBOARD_FILE=<path>` (tests) stands in for the platform tool: text that would go to it is written to that file and the tools never run.
- **Bracketed paste**: the client turns on bracketed paste in the host terminal on attach (`ESC[?2004h`) and off on every exit (detach, session end, error; the terminal is restored by a drop guard). `ESC[200~ … ESC[201~` from the host is one opaque paste, even when split across reads: no prefix, bindings, mouse reports or repeat keys inside it. The text goes to the focused pane, wrapped in `ESC[200~`/`ESC[201~` again only if that pane's app enabled bracketed paste (read from the client's emulator), raw otherwise; paste markers inside the text are stripped. A paste right after the prefix cancels the prefix. With an overlay open, pasted text goes into the rename and new-session prompts (control characters dropped, so a pasted newline doesn't submit) and is ignored everywhere else: it never answers a y/n question or the tmux import offer. Copy mode and the new-tab picker ignore pastes (copy mode says so in the status bar). A read ending in a possible cut-off start marker (`ESC [`, `ESC [ 2`…, or `ESC` after other bytes) waits up to 20 ms for the rest.

## Config
`~/.oui/config.toml` only (no settings UI in v1). Every key is optional; an invalid file is reported in the status bar and the defaults are used. Full reference: [docs/config.example.toml](docs/config.example.toml) (`oui config default`, `oui config check`).
- **tmux import**: `oui config import-tmux [path] [--write] [--force]` translates a tmux.conf subset (prefix, prefix-table binds with an oUI action, `unbind`, mouse, history-limit, default-shell) and lists every skipped line; on first launch, if `config.toml` is missing and a tmux config exists, the client shows an overlay (`SETUP`) with the path and a summary (first imported/skipped lines, "see `oui config import-tmux`" for the rest): `y` writes the config and applies keys, theme, status bar and mouse live (daemon-side `shell`/`scrollback` after `oui kill-server`), `n` skips this time, `d` writes a minimal commented `config.toml` so it isn't offered again. See [docs/tmux-import.md](docs/tmux-import.md).
- `[general]`: `default_profile` (used when `oui`/`oui new` get no `-p`; falls back to system with a warning if missing), `shell` (instead of `$SHELL`), `scrollback` (lines per pane), `mouse` (mouse reporting on the host terminal; see above).
- `[keys]`: `prefix` and `bindings` (tmux-like notation, layered over the defaults; `"none"` removes one). Status bar hints are generated from the actual bindings.
- `theme`: built-in `default`/`nord`/`light`, a file in `~/.oui/themes/`, or a `[theme]` table with `base` + colour overrides. `"profile"` as a colour means the tab's profile colour.
- `[status]`: `left`/`right` segment lists — `session profile mode hints clock cwd hostname tabs-count` or `{ text = "…" }`.
- `[panel]`: `visible`, `width`, `widgets`.
- `[clipboard]`: `apps` = `osc52` | `off` | `system`: what OSC 52 clipboard writes from programs in panes do (see above).

The client reads the config when it starts; the daemon reads `shell` and `scrollback` when it starts (`oui kill-server` to reload).

Prompt integration: [docs/starship.md](docs/starship.md).

## CLI
```
oui                         # open TUI (attach to last session or create one)
oui profile new <name> [--fresh] [--color c] [--keychain secure|file]
oui profile list | share <name> true|false | rm <name>
oui profile keychain <name> secure|file
oui shell <name>            # plain shell in a profile, no TUI
oui run <name> -- <cmd>     # one command in a profile (scripts, CI)
oui attach [session] | ls | kill <session>
oui open <workspace|path>   # create-or-attach a workspace session
oui workspace list | new <name> | save <session> [name] [--force]
```

## Roadmap
1. **Spike (now)** — `oui profile/shell/run/env` working; verify isolation for claude, railway, gh, vercel, git on macOS + Linux. See [docs/isolation-spike.md](docs/isolation-spike.md).
2. **v1 TUI MVP** — daemon, tabs, splits, prefix keys, detach/attach, profiles per tab, status bar, basic right panel, workspaces.
3. **v2** — command palette, deploy guard, folder→profile, resurrection, themes pack, plugins, quick-select, floating panes, terminfo-over-ssh.
4. **v3** — remote attach.
5. **GUI client** — custom design, GPU rendering, fonts, images.

## Open questions
- Linux Secret Service tools: per-profile D-Bus keyring or token adapters?
- License: MIT OR Apache-2.0 (Rust convention) — confirm.
