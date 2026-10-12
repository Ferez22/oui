# oUI

A terminal workspace for macOS and Linux where **every profile has its own logins**.
Two Claude subscriptions, two Railway accounts, two GitHub users — side by side, in separate tabs, never overwriting each other. Plus tmux-style sessions with a modern UI.

> Status: early spike. See [SPEC.md](SPEC.md) for the plan.

## Try the spike
```bash
cargo install --path .
oui profile new work --color green
oui profile new personal --fresh
oui shell work        # now run: claude → /login, railway login, gh auth login ...
oui run personal -- railway whoami
```

Each profile gets its own `HOME` and XDG directories (plus small per-tool adapters), so any CLI's normal login command logs in **for that profile only**.

## License
MIT OR Apache-2.0
