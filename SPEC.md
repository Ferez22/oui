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

## Profiles & login isolation

Layout:
```
~/.oui/
  config.toml
  profiles/<name>/
    profile.toml        # name, color, shared, protected
    home/               # the profile's HOME
      .config/ .local/share/ .local/state/ .cache/ .claude/ ...
```

Every process in a profile gets:

| Variable | Value | Why |
|---|---|---|
| `HOME` | `profiles/<name>/home` | Tools storing logins in `~/.x` (railway, vercel, npm, aws, docker) isolate automatically |
| `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `XDG_STATE_HOME`, `XDG_CACHE_HOME` | inside profile home | Tools using XDG dirs (gh, stripe, many Go/Rust CLIs) |
| `CLAUDE_CONFIG_DIR` | `home/.claude` | Claude Code adapter |
| `GH_CONFIG_DIR` | `home/.config/gh` | gh adapter (explicit) |
| `OUI_PROFILE`, `OUI_PROFILE_COLOR`, `OUI_REAL_HOME` | — | Prompt/Starship integration, tooling |
| `TERM` | `xterm-256color` | Works on every server (fixes nano/vim over ssh) |

Removed from the environment so they can't override profile logins: `ANTHROPIC_API_KEY`, `CLAUDE_CODE_OAUTH_TOKEN`, `RAILWAY_TOKEN`, `RAILWAY_API_TOKEN`, `GH_TOKEN`, `GITHUB_TOKEN`, `VERCEL_TOKEN` (list grows with adapters).

### Shared vs fresh
- **Shared** (default): non-credential dotfiles and toolchains are symlinked from the real HOME (`.zshrc`, `.oh-my-zsh`, `.nvm`, `.cargo`, `starship.toml`, …). Allowlist only — credential locations are never linked.
- **Fresh**: empty home.
- Toggle anytime (`oui profile share <name> true|false`). Unsharing removes only oUI's symlinks; profile logins are kept. UI warns before toggling.

### Adapters (tools that don't follow HOME/XDG)
| Case | Tools | Plan |
|---|---|---|
| macOS Keychain / Linux Secret Service | Claude Code (macOS), git credential helpers, some others | Per-tool adapter. First choice: tool's own namespacing (e.g. Claude Code may key its Keychain entry on `CLAUDE_CONFIG_DIR` — **spike verifies**). Fallback: per-profile long-lived token (`claude setup-token`) stored by oUI in its own Keychain item and injected at spawn. |
| Ignores `HOME` (uses passwd entry) | `ssh` | Per-profile `ssh` wrapper on PATH adding `-F <profile>/home/.ssh/config` when present |
| git identity | git | `GIT_CONFIG_GLOBAL` → profile `.gitconfig` (which can `[include]` the shared one) |

An **isolation test suite** runs each supported tool in two profiles and checks logins don't leak. Contributors add tools to it.

### Guardrails
- Profile color on tab, pane border, status bar.
- `protected = true` profiles ask before risky commands (`railway up`, `vercel --prod`, `git push --force`, …). (v2)

## Sessions (tmux-style)
- Hierarchy: **Session → Tabs → Panes**. Each tab runs one profile; tab label shows profile name + color.
- Prefix key (default `Ctrl-b`), configurable. Default keys after the prefix: `c` new tab, `n`/`p`/`1-9` switch tab, `%`/`|` split right, `"`/`-` split down, arrows/`hjkl` focus, `HJKL` resize, `z` zoom, `x` close pane, `&` close tab, `d` detach, `b` right panel, `[` copy mode, `s` session picker, `,` rename tab; prefix twice sends it to the pane.
- Detach (`prefix d`) / attach (`oui attach <session>`). Shells survive client exit.
- Session manager UI: list, rename, kill, switch.
- **Workspaces**: layout files declaring tabs, panes, profile, cwd, startup command:
  ```toml
  [[tab]]
  profile = "work"
  cwd = "~/code/api"
  panes = [{ cmd = "claude" }, { cmd = "railway logs" }, { cmd = "npm run dev", split = "down" }]
  ```
- Resurrection after reboot (relaunch commands in saved cwd): v2.

## UI
- **Tab bar** (top): profile color + name, session name.
- **Status bar** (bottom): profile, session, mode, key hints, clock. Segments configurable.
- **Right panel** (collapsible, `prefix b`): v1 widgets — Connected accounts (login state per tool per profile, one-key login), Sessions, Profiles. v2: plugin widgets.
- **Starship**: oUI ships a `custom.oui` module showing the active profile.
- Scrollback search, copy mode with vi keys (see below).
- Styling from a theme file; user provides the design.

## Copy mode, mouse, clipboard
- **Scrollback**: each pane's emulator in the daemon keeps `general.scrollback` lines. Clients mirror pane screens and keep their own scrollback as output arrives; entering copy mode (or wheel-scrolling) on a pane fetches the daemon's full history once (`History` request → screen + history replayed into the client's emulator). Keys typed before it arrives are replayed after. The view is anchored: new output grows the history without moving what you're looking at; `[offset/total]` is drawn at the pane's top right.
- **Copy mode** (`prefix [`): vi keys — `hjkl`/arrows, `w b e`, `0 ^ $`, `g G`, `H M L`, `Ctrl-u/d`, `PgUp/PgDn` (also `Ctrl-b/f` when not the prefix), `/` `?` search (lowercase query = case-insensitive, wraps), `n N`, `v` char / `V` line selection, `y`/`Enter` copy + exit, `q`/`Esc` exit. Wrapped lines are joined when copied. Status bar mode badge shows `COPY` and the search prompt. The prefix leaves copy mode and then acts normally. A resize of the pane (new snapshot) leaves copy mode.
- **Mouse** (`general.mouse`, default on): the client enables SGR mouse reporting (1000/1002/1006) on the host terminal. Wheel over a pane enters a scroll view (copy mode, left automatically when scrolled back to the bottom); click focuses a pane, click on a tab selects it; drag selects (scrolling past the pane edge) and copies on release. A full-screen app without mouse support gets arrow keys for the wheel. If a pane's app enabled mouse reporting itself, events go to it instead, re-encoded relative to the pane in the mode (press / press-release / button-motion / any-motion) and encoding (X10, UTF-8, SGR) it asked for; drags stay with the pane they started in. Shift+drag is left to the host terminal (most terminals bypass mouse reporting with Shift). Hover motion (mode 1003) is not requested from the host, so any-motion apps only see motion while a button is held.
- **Clipboard**: copying writes OSC 52 to the host terminal (works over ssh; skipped above 100 KB of text) and pipes into `pbcopy` (macOS) or `wl-copy`/`xclip` (Linux) when present. OSC 52 emitted by apps in panes is caught by the daemon (≤ ~100 KB) and sent to every client attached to the session, which copies it the same way. Clipboard *reads* (OSC 52 `?`) are never answered. `OUI_CLIPBOARD_FILE=<path>` (tests) also writes copied text to that file and disables the platform tools.

## Config
`~/.oui/config.toml` only (no settings UI in v1). Every key is optional; an invalid file is reported in the status bar and the defaults are used. Full reference: [docs/config.example.toml](docs/config.example.toml) (`oui config default`, `oui config check`).
- `[general]`: `default_profile` (used when `oui`/`oui new` get no `-p`; falls back to system with a warning if missing), `shell` (instead of `$SHELL`), `scrollback` (lines per pane), `mouse` (mouse reporting on the host terminal; see above).
- `[keys]`: `prefix` and `bindings` (tmux-like notation, layered over the defaults; `"none"` removes one). Status bar hints are generated from the actual bindings.
- `theme`: built-in `default`/`nord`/`light`, a file in `~/.oui/themes/`, or a `[theme]` table with `base` + colour overrides. `"profile"` as a colour means the tab's profile colour.
- `[status]`: `left`/`right` segment lists — `session profile mode hints clock cwd hostname tabs-count` or `{ text = "…" }`.
- `[panel]`: `visible`, `width`, `widgets`.

The client reads the config when it starts; the daemon reads `shell` and `scrollback` when it starts (`oui kill-server` to reload).

Prompt integration: [docs/starship.md](docs/starship.md).

## CLI
```
oui                         # open TUI (attach to last session or create one)
oui profile new <name> [--fresh] [--color c]
oui profile list | share <name> true|false | rm <name>
oui shell <name>            # plain shell in a profile, no TUI
oui run <name> -- <cmd>     # one command in a profile (scripts, CI)
oui attach [session] | ls | kill <session>
```

## Roadmap
1. **Spike (now)** — `oui profile/shell/run/env` working; verify isolation for claude, railway, gh, vercel, git on macOS + Linux. See [docs/isolation-spike.md](docs/isolation-spike.md).
2. **v1 TUI MVP** — daemon, tabs, splits, prefix keys, detach/attach, profiles per tab, status bar, basic right panel, workspaces.
3. **v2** — command palette, deploy guard, folder→profile, resurrection, themes pack, plugins, quick-select, floating panes, terminfo-over-ssh.
4. **v3** — remote attach.
5. **GUI client** — custom design, GPU rendering, fonts, images.

## Open questions
- Claude Code macOS Keychain behaviour with `CLAUDE_CONFIG_DIR` (spike).
- Linux Secret Service tools: per-profile D-Bus keyring or token adapters?
- License: MIT OR Apache-2.0 (Rust convention) — confirm.
