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
| `Ctrl-b c` (or click `+` in the tab bar) | new tab (pick profile) |
| `Ctrl-b n` / `p` / `1-9` (or click a tab) | next / previous / jump to tab |
| `Ctrl-b %` or `\|` / `"` or `-` | split right / down (same profile as the tab) |
| `Ctrl-b ←↓↑→` or `h j k l` (or click a pane) | move focus between panes; Ctrl/Alt held with the arrow works too, and for 0.6 s more arrows keep moving without the prefix (`REPEAT` in the status bar) |
| `Ctrl-b H J K L` or `Shift-←↓↑→` | move the nearest divider (repeats like focus) |
| `Ctrl-b z` | zoom the focused pane |
| `Ctrl-b d` (or `x`) / `&` | **close the focused split** / close tab. Asks if a program other than the shell is still running. On the very last split it asks "Close oUI?": `y` ends the session, `d` detaches instead, `n` cancels |
| `Ctrl-b D` | detach: asks first, then leaves oUI with your shells running (`oui attach` to come back) |
| `Ctrl-b Ctrl-b` | send a literal Ctrl-b |
| `Ctrl-b b` | show/hide the right panel: connected accounts for this tab's profile, sessions, profiles |
| `Ctrl-b Tab` | focus the panel: `j`/`k` move, `Enter` logs in to the tool (new tab, same profile) / switches session / opens a tab in that profile, `Esc` (or `Ctrl-b Tab`) back |
| `Ctrl-b s` | session picker: `Enter` switch, `n` new, `x` kill (asks y/n) |
| `Ctrl-b ,` | rename the tab (shown as `1 name · profile`; empty clears) |
| `Ctrl-b [` | copy mode: scroll back, search, select, copy (below) |

The prefix and every key above can be changed in the config file (e.g. `D = "none"` to drop detach, or `q = "detach"`). An unbound key after the prefix is ignored entirely (never typed into the pane) and the status bar says so. Confirmations default to No: `y` confirms, anything else (`n`, `Esc`, `Enter`) cancels. Typing `exit` in the last shell ends the session without asking.

The tab bar shows the `oUI` badge, each tab as a block (`1 name · profile`, the active one in its profile colour), a `+` button and the session name (click it for the session picker). With more tabs than fit, it scrolls to keep the active tab visible; `‹N` / `N›` count the hidden ones (click to go there).

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
oui config import-tmux    # translate ~/.tmux.conf (prefix, binds, mouse, history) — prints it
oui config import-tmux --write   # …and save it as ~/.oui/config.toml (--force to replace)
```
Coming from tmux? The first time you start oUI without a `config.toml`, it finds your tmux config and offers to import it: `y` imports (the prefix and keys apply right away; `default-shell` / `history-limit` after `oui kill-server`), `n` asks again next time, `d` writes a minimal config so it never asks again. `import-tmux` carries over your prefix, key bindings for splits/panes/windows, `unbind`s, `mouse`, `history-limit` and `default-shell`, and lists every line it couldn't translate (plugins, styling, `run`, copy-mode tables, root `-n` keys). Details: [docs/tmux-import.md](docs/tmux-import.md).
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

```bash
oui profile list                        # name, colour, shared, keychain mode
oui profile keychain work file          # switch keychain mode (logins are kept)
oui profile rm work                     # delete a profile and its logins (asks first)
```

### macOS: a keychain per profile
Tools that keep logins in the macOS keychain (Claude Code, `gh`, git's `osxkeychain` helper, …) use the *default* keychain, which macOS finds through `HOME`. So every profile gets its own keychain in `~/.oui/profiles/<name>/home/Library/Keychains/`, created by `oui profile new` (or at the first shell for older profiles) and unlocked whenever oUI starts a shell in that profile. Logins saved there are invisible to your other profiles and to your normal environment.

The profile keychain is protected by a random password. Where oUI keeps it is chosen per profile (`oui profile new` asks; or pass `--keychain secure|file`):

- **secure** (default): in your login keychain, as an item named "oUI profile keychain". The profile's keychain stays encrypted at rest. The first time oUI reads that item (after a reboot, or after you upgrade `oui`), macOS may show a dialog asking whether `oui` may use it: that is macOS checking which app wants the password. Choose **Always Allow**; the profile's shells start anyway and the keychain unlocks once you answer.
- **file**: in `~/.oui/profiles/<name>/keychain.pw` (readable only by you). No dialogs ever; the protection is the same as the token files most CLIs keep in your home folder.

If a profile keychain can't be unlocked, the shell still starts and shows a yellow `oUI ▸` warning. On Linux this doesn't apply: CLIs there keep logins in files (isolated by `HOME`/XDG) or in the Secret Service, which oUI doesn't isolate yet.

## License
Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your option.
