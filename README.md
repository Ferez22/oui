# oUI

A terminal workspace for macOS and Linux where **every profile has its own logins**.
Two Claude subscriptions, two Railway accounts, two GitHub users — side by side, in separate tabs, never overwriting each other. Plus tmux-style sessions with a modern UI.

> Status: early spike. See [SPEC.md](SPEC.md) for the plan.

## Try it
```bash
cargo install --path .
oui profile new work --color green
oui profile new personal --fresh
oui new -s main -p work   # open the TUI, first tab in profile "work"
```
Inside a tab, log in as usual (`claude` → `/login`, `railway login`, `gh auth login` …) — the login belongs to that tab's profile.

| Keys | Action |
|---|---|
| `Ctrl-b c` | new tab (pick profile) |
| `Ctrl-b n` / `p` / `1-9` | next / previous / jump to tab |
| `Ctrl-b %` or `\|` / `"` or `-` | split right / down (same profile as the tab) |
| `Ctrl-b ←↓↑→` or `h j k l` | move focus between panes |
| `Ctrl-b H J K L` | move the nearest divider |
| `Ctrl-b z` | zoom the focused pane |
| `Ctrl-b x` / `&` | close pane / close tab |
| `Ctrl-b d` | detach (shells keep running) |
| `Ctrl-b Ctrl-b` | send a literal Ctrl-b |
| `Ctrl-b b` | show/hide the right panel: connected accounts for this tab's profile, sessions, profiles |
| `Ctrl-b Tab` | focus the panel: `j`/`k` move, `Enter` logs in to the tool (new tab, same profile) / switches session / opens a tab in that profile, `Esc` (or `Ctrl-b Tab`) back |
| `Ctrl-b s` | session picker: `Enter` switch, `n` new, `x` kill (asks y/n) |
| `Ctrl-b ,` | rename the tab (shown as `1 name · profile`; empty clears) |
| `Ctrl-b [` | copy mode: scroll back, search, select, copy (below) |

The prefix and every key above can be changed in the config file.

### Copy mode, mouse, clipboard
`Ctrl-b [` opens the focused pane's scrollback (`general.scrollback` lines, default 10000); `[offset/total]` shows the position and output keeps arriving without moving the view.

| Copy mode keys | Action |
|---|---|
| `h j k l` / arrows | move |
| `w` `b` `e` | next word / previous word / end of word |
| `0` `^` `$` | line start / first non-blank / line end |
| `g` / `G` | top of history / bottom |
| `H` `M` `L` | top / middle / bottom of the view |
| `Ctrl-u` / `Ctrl-d`, `PgUp` / `PgDn` | half page / page up and down |
| `/` `?` then `n` / `N` | search forward / backward, next / previous (lowercase = ignore case) |
| `v` / `V` | start character / line selection |
| `y` or `Enter` | copy the selection and leave |
| `q` / `Esc` | leave (the prefix also leaves, then works as usual) |

With `general.mouse = true` (default): the wheel scrolls a pane's history (scrolling back to the bottom leaves), click focuses a pane or selects a tab, drag selects text and copies it on release. Apps that use the mouse themselves (vim with `mouse=a`, htop, …) get the mouse events instead. Hold **Shift** to use your terminal's own selection (Option/Alt in some macOS terminals). The mouse is ignored over the right panel and while a picker or prompt is open.

Copying uses OSC 52, so it reaches your local clipboard even over ssh (enable it in your terminal if needed; payloads over 100 KB are skipped), plus `pbcopy` / `wl-copy` / `xclip` when installed. Apps in a pane that set the clipboard with OSC 52 (e.g. neovim, tmux inside oUI) go through the same path.

```bash
oui ls                    # list sessions
oui attach [session]      # reattach (default: most recent)
oui kill <session>        # end a session
oui shell work            # plain shell in a profile, no TUI
oui run work -- railway whoami
```

## Workspaces
Declare a session in `~/.oui/workspaces/<name>.toml`: tabs, each tab's profile and directory, and its split panes with startup commands.
```toml
name = "api"
cwd = "~/code/api"
[[tab]]
profile = "work"
layout = { split = "right", panes = [
  { cmd = "claude" },
  { split = "down", panes = [{ cmd = "railway logs" }, { cmd = "npm run dev" }] },
] }
[[tab]]
profile = "personal"
```
```bash
oui workspace new api          # commented template to edit
oui open api                   # create the session, or attach if it's running
oui workspace list
oui workspace save main api    # save a running session's layout
```
Commands are typed into the pane's shell, so the shell stays when they exit. Details: [docs/workspaces.md](docs/workspaces.md).

## Config
`~/.oui/config.toml` (all keys optional): prefix and key bindings, theme (`default`, `nord`, `light` or your own in `~/.oui/themes/`), status bar segments, right panel, default profile, shell, scrollback.
```bash
oui config default        # print the full default config, commented
oui config check          # validate your config
```
See [docs/config.example.toml](docs/config.example.toml) for every option, e.g.:
```toml
theme = "nord"
[general]
default_profile = "work"   # used by `oui` / `oui new` without -p
[keys]
prefix = "C-a"
[keys.bindings]
v = "split-right"
[status]
left = ["mode", "session", "profile"]
right = ["hints", "clock"]
```
Show the active profile in your prompt: [docs/starship.md](docs/starship.md).

## Profiles
Shared profiles link your non-credential dotfiles (shell config, prompt, toolchains). Add more per profile in `~/.oui/profiles/<name>/profile.toml`:
```toml
extra_shared = [".config/foo", ".mytool/config"]
```

Each profile gets its own `HOME` and XDG directories (plus small per-tool adapters), so any CLI's normal login command logs in **for that profile only**.

## License
Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your option.
