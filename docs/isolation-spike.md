# Isolation spike

Goal: prove each tool keeps a separate login per profile.

## Build
```bash
cargo install --path .
```
(macOS: if linking fails with an Xcode error, prefix with `DEVELOPER_DIR=/Library/Developer/CommandLineTools`.)

## Results so far
| Tool | Fresh profile sees no login | Two logins coexist | Notes |
|---|---|---|---|
| gh | ✅ "not logged into any GitHub hosts" | ⏳ | |
| railway | ✅ "Unauthorized. Please login" | ⏳ | |
| vercel | ✅ (prompts to log in) | ⏳ | `whoami` waits for input when logged out |
| claude | ⏳ | ⏳ | **Key test** — Keychain on macOS |
| git identity | ⏳ | ⏳ | needs `GIT_CONFIG_GLOBAL` adapter |

## Manual test: Claude Code (most important)
Uses your real accounts, so run it yourself.

1. `oui profile new a` and `oui profile new b`
2. Terminal tab 1: `oui shell a` → `claude` → `/login` with subscription 1 → `/status`, note the account.
3. Terminal tab 2: `oui shell b` → `claude` → `/login` with subscription 2.
4. Back in tab 1: quit and start `claude` again → `/status`. **Still subscription 1?**
5. In a normal terminal (no profile): `claude` → `/status`. Your original login should be untouched.
6. Open the Keychain Access app and search "Claude Code". One entry per profile, or one shared entry?

Report the results of steps 4–6. If step 4 shows subscription 2, we build the token adapter (see SPEC "Adapters").

## Same test for railway / gh / vercel
Same steps with `railway login` + `railway whoami`, `gh auth login` + `gh auth status`, `vercel login` + `vercel whoami`.
