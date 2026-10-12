# What the best terminals do (research for oUI)

Survey of features from leading terminals and multiplexers, used to build the oUI feature list in [SPEC.md](../SPEC.md).
"oUI" column: **v1** = TUI MVP, **v2** = later TUI, **GUI** = needs the future GUI client, **–** = skip.

## Terminals

| Terminal | Standout features | Worth copying |
|---|---|---|
| **Ghostty** | Native UI per OS, GPU rendering, fast; 300+ built-in themes; quick terminal (hotkey drop-down); shell integration (prompt marks, cwd tracking); kitty graphics protocol | Built-in themes, shell integration, quick terminal (GUI) |
| **WezTerm** | Lua config; **built-in multiplexer** with domains (local, SSH, unix socket); launcher menu; quick-select mode (keyboard-pick URLs/hashes/paths); copy mode | Domains model for remote, quick-select, copy mode |
| **Kitty** | Graphics protocol; "kittens" (extensions); remote control API; hints mode; **`kitten ssh` copies terminfo to the server automatically** | Auto-terminfo over SSH (fixes the nano problem), remote-control API |
| **Warp** | Blocks (each command + output is a selectable unit); IDE-like input editor; command palette; saved workflows; AI | Blocks, command palette, workflows |
| **iTerm2** | Profiles with **automatic switching** by host/dir; badges (big label on the pane); triggers (regex → action); tmux `-CC` integration; instant replay; shell integration marks | Auto profile switching, badges, triggers |
| **Wave Terminal** | **Side panels / blocks** next to the terminal (file preview, web, widgets) | Customizable right panel |
| **Alacritty** | Minimal, very fast, vi mode | Vi mode for scrollback |
| **Tabby** | SSH connection manager, saved hosts, serial | SSH host manager (v2) |

## Multiplexers

| Tool | Standout features | Worth copying |
|---|---|---|
| **tmux** | Prefix key, detach/attach, copy mode, scriptable, persistent server | Prefix keybindings, detach/attach, scripting |
| **Zellij** | Layouts in KDL; WASM plugins; floating panes; status bar with key hints; session manager UI; session resurrection; web client | Layout files, floating panes, key-hint status bar, session manager UI |

## Prompt / bar

| Tool | Notes |
|---|---|
| **Starship** | Cross-shell prompt, TOML config, modules. oUI ships a `custom.oui` Starship module showing the active profile (reads `$OUI_PROFILE`). |

## oUI feature list derived from this

| Feature | From | oUI |
|---|---|---|
| Profiles with isolated logins (HOME/XDG + adapters) | oUI core | v1 |
| Tabs labelled + coloured by profile | iTerm2 badges | v1 |
| Splits, prefix keybindings (tmux-style) | tmux | v1 |
| Detach/attach, sessions survive closing the client | tmux, Zellij | v1 |
| Saved workspaces / layout files (panes + startup commands) | Zellij layouts | v1 |
| Status bar with profile, session, key hints | Zellij | v1 |
| Collapsible, customizable right panel (accounts, sessions, widgets) | Wave | v1 (basic), v2 (plugins) |
| Connected-accounts panel (login state per tool) | oUI | v1 |
| `TERM=xterm-256color` default; optional terminfo install over SSH | Kitty | v1 / v2 |
| Starship profile module | Starship | v1 |
| Scrollback search, copy mode, vi mode | tmux, Alacritty | v1 |
| Clickable links + quick-select (URLs, paths, hashes) | WezTerm | v2 |
| Command palette | Warp | v2 |
| Floating panes | Zellij | v2 |
| Folder → profile auto-switch | iTerm2 auto-switching | v2 |
| Deploy guard (confirm risky commands in protected profiles) | iTerm2 triggers | v2 |
| Session resurrection after reboot (rerun commands in saved cwd) | Zellij | v2 |
| Blocks (command + output units) | Warp | v2 (needs shell integration) |
| Built-in theme pack, import iTerm/Ghostty themes | Ghostty | v2 |
| Plugins (panel widgets, status-bar segments) | Zellij, Kitty | v2 |
| Remote attach / SSH domains / web client | WezTerm, Zellij | v3 |
| GPU rendering, fonts, ligatures, images, quick terminal | Ghostty, Kitty | GUI |
