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

```bash
oui ls                    # list sessions
oui attach [session]      # reattach (default: most recent)
oui kill <session>        # end a session
oui shell work            # plain shell in a profile, no TUI
oui run work -- railway whoami
```

Shared profiles link your non-credential dotfiles (shell config, prompt, toolchains). Add more per profile in `~/.oui/profiles/<name>/profile.toml`:
```toml
extra_shared = [".config/foo", ".mytool/config"]
```

Each profile gets its own `HOME` and XDG directories (plus small per-tool adapters), so any CLI's normal login command logs in **for that profile only**.

## License
Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your option.
