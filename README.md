# oUI

**One terminal, many accounts.** oUI is a terminal workspace for macOS and Linux where every **profile** has its own logins. Run two Claude subscriptions, two Railway accounts or two GitHub users side by side in separate tabs. They never overwrite each other.

It also works like tmux: tabs, split panes, sessions that keep running when you close the window, a clickable tab bar and a side panel showing which accounts each profile is logged into.

> Status: early (v1). macOS is the main target; Linux works except for keychain isolation (see [Limitations](#limitations)).

---

## Install

You need [Rust](https://rustup.rs). Then:

```bash
git clone https://github.com/Ferez22/oui && cd oui
cargo install --path .
```

**Updating:** run `oui kill-server` first (this closes your running oUI sessions and their shells), then `cargo install --path . --force` again.

---

## Quick start (2 minutes)

1. **Create a profile per account.** A profile is just a name; its logins live in their own folder.
   ```bash
   oui profile new work --color green
   oui profile new personal --color magenta
   ```
   On macOS you're asked how to protect the profile's keychain; press Enter for the default ([what this means](#the-keychain-password-prompt)).

2. **Open oUI** with a first tab in the `work` profile:
   ```bash
   oui new -s main -p work
   ```
   Coming from tmux? On the very first start oUI offers to import your `~/.tmux.conf` (prefix, key bindings, mouse). Press `y` to import, `n` to skip.

3. **Log in as usual.** In that tab run `claude` → `/login`, `railway login`, `gh auth login`… The login is saved **for the `work` profile only**.

4. **Open a tab in another profile:** press `Ctrl-b` then `c` (or click `+` in the tab bar) and pick `personal`. Log in there with your other account.

5. **Check it:** `Ctrl-b` then `b` opens the side panel, which lists which tools each profile is logged into.

Every tab shows its profile name and colour, so you always know which account you're using.

---

## How profiles work

Command-line tools save their logins in your home folder (`~/.railway`, `~/.config/gh`, `~/.claude`…). oUI gives each profile **its own home folder** (`~/.oui/profiles/<name>/home`), so each tool's normal login command saves into that profile only. Nothing to configure per tool.

| | Separate per profile | Shared with your normal setup |
|---|---|---|
| Logins and tokens | ✅ | |
| macOS keychain (Claude Code, gh, git…) | ✅ its own keychain | |
| **Claude Code chat history** (`claude --resume`) | ✅ each profile has its own | |
| Shell history | ✅ | |
| cargo login (`~/.cargo/credentials.toml`) | ✅ each profile has its own `~/.cargo` | `~/.cargo/bin` and `~/.cargo/env` are linked |
| ssh keys | ✅ once the profile has its own `~/.ssh/config` ([how](#a-different-ssh-key-per-profile-eg-github)) | otherwise ssh uses your normal keys |
| Shell config, prompt, toolchains (`.zshrc`, `.p10k.zsh`, `.oh-my-zsh`, `.nvm`, `.rustup`…) | | ✅ linked, unless the profile was created with `--fresh` |

- `oui profile new <name>` creates a **shared** profile: your shell setup is linked in, logins are not.
- `oui profile new <name> --fresh` creates an **empty** profile: nothing linked.
- `system` is your normal environment, with no isolation. You can pick it for a tab too.

```bash
oui profile list                 # your profiles
oui accounts work                # which tools the "work" profile is logged into
oui shell work                   # a plain shell in a profile, without the oUI interface
oui run work -- railway whoami   # one command in a profile
oui profile rm work              # delete a profile and its logins (asks you to type its name)
```

To share more of your own files into a profile, list them in `~/.oui/profiles/<name>/profile.toml` (paths relative to your home, without `..`; `Library/Keychains` can't be shared):
```toml
extra_shared = [".config/foo"]
```

A profile's folders are private to you: `~/.oui` and the folders in it are mode 0700 and its files 0600, like the `~/Library` they stand in for. Folders made by older versions are tightened the next time a shell starts in the profile.

In a shared profile, cargo's own settings (`~/.cargo/config.toml`) aren't linked because they can hold registry tokens, and downloaded crates are kept per profile. Copy the config into the profile's `~/.cargo` if you need it.

### A different ssh key per profile (e.g. GitHub)
`ssh` ignores the profile's home folder, so by default every profile uses your normal ssh keys (and pushes to GitHub as the same user). To give a profile its own key:

```bash
oui shell work
mkdir -m 700 -p ~/.ssh
ssh-keygen -t ed25519 -f ~/.ssh/id_ed25519      # ~ is the profile's home here
cat >> ~/.ssh/config <<EOF
Host github.com
  IdentityFile $HOME/.ssh/id_ed25519
  IdentitiesOnly yes
EOF
```
(`$HOME` is written out as the full path on purpose: ssh would read `~` as your normal home.) Add `~/.ssh/id_ed25519.pub` to the GitHub account, then open a new tab in the profile.

From then on, shells in that profile get an `ssh` wrapper that uses this config (`ssh -F <profile>/home/.ssh/config`), and git uses it for every push and pull (`GIT_SSH_COMMAND`). On macOS, login shells reorder `PATH` (`/etc/zprofile`), which puts the system `ssh` ahead of the wrapper when you type `ssh` yourself. Add this line to your `~/.zshrc` (it does nothing outside profiles):
```bash
if [ -n "$OUI_PROFILE_BIN" ]; then export PATH="$OUI_PROFILE_BIN:$PATH"; fi
```

### The keychain password prompt
On macOS, tools like Claude Code store logins in the **keychain**, not in files. So oUI gives every profile its own keychain, protected by a long random password that oUI manages. **You never need to know or type that password.**

Where oUI keeps it is chosen per profile when you create it:
- **secure** (default): in your normal macOS login keychain, as an item called **"oUI profile keychain"**.
- **file**: in a file only you can read (`~/.oui/profiles/<name>/keychain.pw`). No macOS prompts ever.

**What you'll see with `secure`:** after a restart, or after you install a new version of oUI, macOS may ask:

> *"oui wants to use your confidential information stored in 'oUI profile keychain' in your keychain."*

That's macOS checking that the oUI app may read its own password. Type **your Mac login password** and click **Always Allow**. Because oUI isn't code-signed yet, macOS asks again after every update.

**If instead a dialog asks for the password of a keychain called "oui",** press **Cancel**, then open a new tab in that profile. oUI unlocks it automatically; that dialog appears when a tool touches the keychain before oUI has finished unlocking it.

To switch a profile between the two modes, use `oui profile keychain <name> secure|file`. Your logins are kept.

---

## Everyday keys

Press the **prefix** (`Ctrl-b`, or your tmux prefix if you imported it), release, then the key.

| Keys | What it does |
|---|---|
| `c` (or click `+`) | new tab, pick its profile |
| `n` / `p` / `1`–`9` (or click a tab) | next / previous / go to tab |
| `%` or `\|` | split right |
| `"` or `-` | split down |
| arrows or `h j k l` (or click a pane) | move between splits |
| `z` | zoom the current split (again to unzoom) |
| `d` (or `x`) | **close the current split.** On the last one oUI asks *"Close oUI?"*: `y` close, `d` detach instead, `n` cancel |
| `&` | close the whole tab |
| `D` | detach: oUI closes, **your shells keep running** (`oui attach` to come back) |
| `b` | show/hide the side panel (accounts, sessions, profiles) |
| `s` | session picker |
| `,` | rename the tab |
| `[` | scroll back / copy mode |

The status bar at the bottom always shows the main keys. [Full key reference](#full-key-reference) below.

---

## Sessions

A session is a set of tabs. It keeps running in the background when you detach (`D`) or close the terminal window.

```bash
oui ls                 # list running sessions
oui attach             # reopen the most recent one (or: oui attach main)
oui kill main          # end a session
```

Sessions do **not** survive a reboot or `oui kill-server`. To get the same layout back quickly, save it as a [workspace](#workspaces).

---

## Workspaces

A workspace file describes a session: its tabs, each tab's profile and folder, and its splits with the command each one starts.

```toml
# ~/.oui/workspaces/api.toml
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
oui open api                    # start it (or reattach if it's already running)
oui workspace new api           # create a commented template
oui workspace save main api     # save a running session's layout as a workspace
oui workspace list
```

Commands are typed into a normal shell, so when they exit you're left at a prompt. Only open workspace files you trust: they run commands. More: [docs/workspaces.md](docs/workspaces.md).

---

## Config

Everything is optional and lives in `~/.oui/config.toml`.

```bash
oui config default              # print every option, with comments
oui config check                # check your config for mistakes
oui config import-tmux --write  # import your tmux config (prefix, keys, mouse, history)
```

Example:
```toml
theme = "nord"                  # default, nord, light, or your own in ~/.oui/themes/
[general]
default_profile = "work"        # profile for `oui` / `oui new` without -p
[keys]
prefix = "C-a"
[keys.bindings]
v = "split-right"
```

All options: [docs/config.example.toml](docs/config.example.toml). tmux import details: [docs/tmux-import.md](docs/tmux-import.md). Show the profile in your prompt: [docs/starship.md](docs/starship.md).

---

## Troubleshooting

**"My old Claude sessions are gone when I run `claude --resume` in a profile."**
They aren't gone. Each profile keeps its own Claude history, so sessions you ran outside oUI (or in another profile) aren't listed there. Open a normal terminal and run `claude --resume` in that folder, or copy the history into the profile:
```bash
cp -R ~/.claude/projects/<folder-name>/. ~/.oui/profiles/<profile>/home/.claude/projects/<folder-name>/
```
(`<folder-name>` is the project path with `/` replaced by `-`, e.g. `-Users-you-code-api`.)

**"macOS asks for a keychain password."** See [The keychain password prompt](#the-keychain-password-prompt).

**"I closed my window and my session is gone."** It's probably still running: `oui ls`, then `oui attach`. After a reboot or `oui kill-server`, sessions are gone; use [workspaces](#workspaces) to recreate them.

**"I'm stuck in a split."** Prefix then an arrow key moves between splits, or just click the other split. Prefix then `d` closes the current one.

**"Something looks broken after an update."** Run `oui kill-server` and start `oui` again. The background server keeps running the old version until you do. If the versions can't talk to each other, `oui` says so ("oUI was updated but an older background server is still running") and does nothing else; `oui kill-server` still stops the old server. `oui --version` shows the version and protocol.

**"Splitting says no room to split."** Every split needs at least 2 columns and 1 row; close a split or make the window bigger.

**"`nano`/`vim` look wrong over ssh."** oUI uses `TERM=xterm-256color`, which every server knows. If you changed `TERM` yourself, set it back.

---

## Limitations

- **Profiles separate accounts; they are not a sandbox.** All profiles run as your macOS user, so a program in one profile could still read another profile's files if it went looking. The goal is that your tools never mix up accounts, not to contain malicious software.
- **Still shared between profiles** (these don't go through the home folder):
  - the **ssh agent** (`SSH_AUTH_SOCK`): keys added with `ssh-add` are offered in every profile unless the profile's `~/.ssh/config` says `IdentitiesOnly yes`. `~/.ssh/known_hosts` is your normal one.
  - the **GPG agent** and its keys (signed commits).
  - **macOS app preferences** (`defaults`, apps' settings) and anything else that talks to a background service of your user.
  - on Linux, the **Secret Service** (GNOME Keyring/KWallet).
  - your **linked shell config**: a token exported in `.zshrc` (or a fish universal variable, `set -Ux`) reaches every shared profile. oUI removes known token variables (`GH_TOKEN`, `CARGO_REGISTRY_TOKEN`, `NPM_TOKEN`…) when it starts a shell, but your shell config runs after that.
- **Linux:** logins stored in files are isolated per profile, but logins stored in the Linux Secret Service are not yet (see above).
- Sessions don't survive a reboot (use workspaces).

---

## Full key reference

After the prefix:

| Keys | Action |
|---|---|
| `c` / click `+` | new tab (pick profile) |
| `n` / `p` / `1-9` / click a tab | next / previous / jump to tab |
| `%` `\|` / `"` `-` | split right / down (same profile as the tab) |
| arrows or `h j k l` / click | move focus. Holding Ctrl or Alt with the arrow works too, and for 0.6 s more arrows keep moving without the prefix (`REPEAT` in the status bar) |
| `H J K L` or Shift+arrows | move the nearest divider |
| `z` | zoom the focused split |
| `d` or `x` | close the focused split (asks if a program is still running, or "Close oUI?" on the very last one) |
| `&` | close the tab |
| `D` | detach (asks first) |
| `b` | show/hide the side panel |
| `Tab` | move into the side panel: `j`/`k` move, `Enter` logs in to a tool (opens a tab in that profile) / switches session / opens a tab in that profile, `Esc` back |
| `s` | session picker: `Enter` switch, `n` new, `x` kill |
| `,` | rename the tab (shown as `1 name · profile`; empty clears) |
| `[` | copy mode |
| prefix twice | send the prefix key itself to the program |

Every key can be changed in the config (e.g. `D = "none"` removes detach). Unknown keys after the prefix are ignored, never typed into your shell. Confirmations default to **No**: only `y` confirms.

**Copy mode** (`[`): scroll back through the pane's history (10,000 lines by default).

| Keys | Action |
|---|---|
| `h j k l` / arrows, `w` `b` `e`, `0` `^` `$` | move by character, word, line |
| `g` / `G`, `H` `M` `L` | top / bottom of history, top / middle / bottom of the view |
| `Ctrl-u` `Ctrl-d`, `PgUp` `PgDn` | half page / page |
| `/` `?` then `n` / `N` | search forward / backward, next / previous |
| `v` / `V` | select characters / lines |
| `y` or `Enter` | copy and leave |
| `q` or `Esc` | leave |

**Mouse:** scroll wheel scrolls history, click focuses a split or tab, drag selects and copies. Programs that use the mouse themselves (vim, htop) get it instead. Hold **Shift** to use your terminal's own selection.

**Clipboard:** copying works locally and over ssh (OSC 52), and also uses `pbcopy` / `wl-copy` / `xclip` when available.

---

## License
Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your option.
