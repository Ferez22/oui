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
| `Ctrl-b Tab` | focus the panel: `j`/`k` move, `Enter` logs in to the tool (new tab, same profile) / switches session / opens a tab in that profile, `Esc` back |
| `Ctrl-b s` | session picker: `Enter` switch, `n` new, `x` kill (asks y/n) |
| `Ctrl-b ,` | rename the tab (shown as `1 name · profile`; empty clears) |
| `Ctrl-b [` | copy mode (coming soon) |

The prefix and every key above can be changed in the config file.

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
