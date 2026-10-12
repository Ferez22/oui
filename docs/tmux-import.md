# Importing a tmux config

oUI can translate the parts of your tmux config that have an oUI equivalent
into `~/.oui/config.toml`.

```bash
oui config import-tmux                  # find your tmux.conf, print the result
oui config import-tmux path/to/tmux.conf
oui config import-tmux --write          # save as ~/.oui/config.toml
oui config import-tmux --write --force  # replace an existing config.toml
```

Without a path, oUI looks where tmux does: `$XDG_CONFIG_HOME/tmux/tmux.conf`,
`~/.config/tmux/tmux.conf`, then `~/.tmux.conf`. The summary (what was imported, what was
skipped and why) goes to stderr and the TOML to stdout, so
`oui config import-tmux > my.toml` works too. `--write` never replaces an existing
config unless you add `--force`; to merge, print the result and copy its `[keys]`
section into your config by hand.

Your tmux config is only read, never changed.

## What is translated

| tmux | oUI |
|---|---|
| `set -g prefix C-a` (`set-option`, `^A` form too) | `keys.prefix = "C-a"` |
| `set -g mouse on\|off` | `general.mouse` |
| `set -g history-limit N` | `general.scrollback` |
| `set -g default-shell /bin/zsh` | `general.shell` |
| `set -g base-index N` | used to number tabs for `select-window -t N` |
| `bind <key> split-window -h` / `-v` (`-c dir` ignored) | `split-right` / `split-down` |
| `bind <key> select-pane -L\|-R\|-U\|-D` | `focus-left/right/up/down` |
| `bind [-r] <key> resize-pane -L\|-R\|-U\|-D [n]` | `resize-left/right/up/down` (amount and repeat ignored) |
| `bind <key> resize-pane -Z` | `zoom` |
| `bind <key> new-window [-c dir]` | `new-tab` |
| `bind <key> next-window` / `previous-window` | `next-tab` / `prev-tab` |
| `bind <key> select-window -t N` | `select-tab-K` (K = N − base-index + 1, 1–9) |
| `bind <key> kill-pane` / `kill-window` (also inside `confirm-before`) | `close-pane` / `close-tab` |
| `bind <key> detach[-client]` | `detach` |
| `bind <key> copy-mode` | `copy-mode` |
| `bind <key> choose-session` / `choose-tree [-s] [-Z]` | `session-picker` |
| `bind <key> command-prompt … "rename-window '%%'"` | `rename-tab` |
| `bind <key> send-prefix` | `send-prefix` |
| `unbind <key>` | `"<key>" = "none"` if oUI binds that key by default |
| `unbind -a` | every default binding set to `"none"` |

Short command names (`splitw`, `selectp`, `resizep`, `neww`, `next`, `prev`, `selectw`,
`killp`, `killw`) and `bind-key -T prefix …` work too. Later lines win, as in tmux.

Key names are converted to oUI notation: `C-a`, `M-x`, `S-Left`, `^a` → `C-a`,
`Escape` → `Esc`, `PPage`/`NPage` → `PageUp`/`PageDown`, `IC`/`DC` → `Insert`/`Delete`,
`Enter`, `Space`, `Tab`, `BTab`, `BSpace`, `F1`–`F12`, quoted characters like `'"'` and
`"%"`, and `\;`.

## What is skipped

Every other line is listed in the summary and in a comment at the top of the generated
file, with the reason — nothing is dropped silently:

- plugins and their options (`set -g @plugin …`, `set -g @anything …`) and `run`/`run-shell`;
- status bar, colours and styles (`status-*`, `*-style`, `*-format`) — use oUI's
  `theme` and `[status]` instead;
- window options (`setw`, `set-window-option`, e.g. `mode-keys vi`);
- other key tables: copy-mode tables (`-T copy-mode-vi`) and root bindings without the
  prefix (`bind -n`);
- several commands on one line or bound to one key (`\;`, `;`), `{ … }` blocks,
  `if-shell`, `source-file`, `%if` directives;
- commands with options oUI can't honour (e.g. `split-window -b`, `split-window -h htop`,
  `new-window -n name`) and anything else without an oUI action.

Bindings that run a shell command, popups, menus and prefix2 have no oUI equivalent yet.
