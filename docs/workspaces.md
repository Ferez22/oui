# Workspaces

A workspace is a TOML file that declares a session: its tabs, the profile and
directory of each tab, and how each tab is split into panes, with an optional
command to start in each pane.

```bash
oui workspace new api        # write a commented template to ~/.oui/workspaces/api.toml
$EDITOR ~/.oui/workspaces/api.toml
oui open api                 # create the session "api" — or attach if it's running
oui open ./other.toml        # any path works too (contains "/" or ends in .toml)
oui workspace list           # workspaces in ~/.oui/workspaces
oui workspace save main api  # save the running session "main" as workspace "api"
```

## Format

```toml
name = "api"              # session name (default: the file name)
cwd = "~/code/api"        # default directory for every tab

[[tab]]
profile = "work"          # a profile from `oui profile list`, or "system"
cwd = "~/code/api"        # optional, overrides the default
layout = { split = "right", ratio = 0.5, panes = [
  { cmd = "claude" },
  { split = "down", panes = [
    { cmd = "railway logs" },
    { cmd = "npm run dev", cwd = "web" },
  ] },
] }

[[tab]]
profile = "personal"      # no layout: one plain shell
```

gives tab 1 (profile `work`):

```
┌──────────┬──────────────┐
│          │ railway logs │
│  claude  ├──────────────┤
│          │ npm run dev  │
└──────────┴──────────────┘
```

and tab 2, a shell in profile `personal`. See
[workspace.example.toml](workspace.example.toml) for a complete file.

### Keys

| Where | Key | Meaning |
|---|---|---|
| top | `name` | Session name. Default: the file name without `.toml`. |
| top | `cwd` | Default directory for tabs. Default: where you run `oui open`. |
| `[[tab]]` | `profile` | Profile every pane of the tab runs in. `"system"` (or omitted) = your normal environment. |
| `[[tab]]` | `cwd` | The tab's directory. Relative paths are relative to the top-level `cwd`. |
| `[[tab]]` | `layout` | A pane or a split (below). Omitted: one plain shell. |
| pane | `cmd` | Startup command (see below). Omitted: plain shell. `{}` is a plain shell. |
| pane | `cwd` | The pane's directory, relative to the tab's. |
| split | `split` | `"right"` (side by side) or `"down"` (stacked). |
| split | `panes` | Two or more panes or splits. |
| split | `ratio` | Share of the first pane, 0.1–0.9. Default: equal shares. With more than two panes, the rest share the remaining space equally. |

`~` and `~/…` expand to your real home (not a profile's home).

The layout uses TOML 1.1 multi-line inline tables (newlines and trailing commas
inside `{ … }`). oUI reads them; if your editor's TOML plugin complains, the same
layout also works on a single line.

### Startup commands

oUI starts the pane's shell as usual (login shell, profile environment) and then
**types** the command into it, followed by Enter — exactly as if you had typed
it. So:

- when the command exits (or you press Ctrl-C), you're back at a shell prompt
  in the same pane, and the command is in the shell's history (↑ to rerun);
- shell aliases and functions work;
- the command may appear before the first prompt is drawn, because it is typed
  while the shell is still starting.

## Opening

`oui open <name|path>` reads and checks the file first, in your terminal: TOML
errors, unknown keys, unknown profiles and missing directories are reported with
the file and line, and nothing is created. Then:

- if a session with the workspace's `name` is running, oUI attaches to it
  (the file is not applied again — kill the session to start fresh);
- otherwise it creates the session with every tab and pane, starts the
  commands, and attaches with tab 1 active.

## Saving

`oui workspace save <session> [name]` writes the current shape of a running
session to `~/.oui/workspaces/<name>.toml` (default name: the session's). It
records each tab's profile, the split tree with its ratios, and each shell's
current directory. Running commands are not captured — add `cmd = "…"` to the
panes you want started. It refuses to overwrite an existing file unless you pass
`--force`.
