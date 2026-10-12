# oUI — notes for Claude

## Launch / promotion plan
When the user asks where to post or how to launch, use this:

**Before announcing**
- Owner has verified the core flow on their Mac: two Claude subscriptions in two profiles, both still logged in after a restart.
- Fresh `brew install ferez22/tap/oui` works end to end.
- Code signing + notarization (owner has an Apple Developer account) so macOS stops re-asking keychain permission after every update.

**Where to post**
- **Show HN** (news.ycombinator.com/submit): title like "Show HN: oUI – a terminal where each profile has its own logins (two Claude accounts side by side)". Link the GitHub repo; first comment explains the why, how isolation works (per-profile HOME/XDG + per-profile macOS keychain), and limits (v0.1, macOS-first, not a sandbox).
- **Reddit**: r/commandline, r/ClaudeAI (people with several Claude subscriptions), r/rust (built in Rust, ratatui), r/tmux (tmux-style, imports tmux.conf), r/macapps.
- **X / Twitter** and **LinkedIn**: short demo video (switching profile tabs, two Claude accounts, the accounts panel).
- **Product Hunt**: later, once the website and demo video exist.
- **Homebrew core** (`brew install oui` without the tap): submit once the project has traction (~75+ stars, not brand new, stable release). The name `oui` is free in homebrew-core as of 2026-10-12.

**Messaging tips**
- Lead with the problem: "two Claude/Railway/GitHub accounts, one terminal, no logging out".
- Be upfront: v0.1, macOS-first (Linux works except keychain isolation), profiles separate accounts but are not a sandbox.
- A 20–30 s demo video does most of the work.

## Release process
- Push a `v*` tag on `main` → `.github/workflows/release.yml` builds macOS (arm64, x86_64) + Linux (x86_64, arm64), creates the GitHub release, and updates `Ferez22/homebrew-tap` if the `HOMEBREW_TAP_TOKEN` secret is set (otherwise render with `scripts/render-formula.sh <version> <dir-with-sha256-files>` and push `Formula/oui.rb` to the tap by hand).
- After upgrading, users must run `oui kill-server` (protocol version check refuses mismatched daemons).

## Website
Next.js site in `website/` (hero, install command, social links, GitHub link). The user will provide a motion/UI prompt for animations.
