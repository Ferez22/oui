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
| claude | ⏳ | ⏳ | macOS: `/login` failed ("Couldn't save your login… keychain", "keychain not found") — the profile HOME had no keychain. Fixed by per-profile keychains (below); re-test steps 4–6. |
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

## Result: macOS keychain needs a per-profile keychain
First real test: inside a profile, `claude` → `/login` failed with "OAuth error: Couldn't save your login. If your Mac's keychain is locked…" (system message: keychain not found). macOS resolves the default keychain via `$HOME`, and the fake HOME had no `Library/Keychains/login.keychain-db`.

Experiments (throwaway keychains under a temp dir, fake HOMEs only):
- A keychain at `<fake home>/Library/Keychains/login.keychain-db` becomes the default keychain of processes with `HOME=<fake home>`; `security add-generic-password` stores into it and the real login keychain doesn't see the item. So a keychain per profile isolates every keychain-based tool with no per-tool code.
- But a keychain **named** `login.keychain(-db)` can't be unlocked with its own password once locked (securityd validates "login" keychains against the account's keybag: `validatePassphraseAndGenerateDerivedEntropy … failed`). A keychain with another name works, and a `login.keychain-db` **symlink** to it is still picked up as the default — that's what oUI creates (`oui.keychain-db` + symlink).
- `security default-keychain -s` / `list-keychains -s` under a fake HOME don't persist anything there, so the symlink is the only reliable way to set a profile's default.
- Creating a keychain (Security framework or `security create-keychain`) adds it to the **creating process's HOME** search list. Created from the real HOME, it joined the real search list (oUI now creates with `HOME` = the profile home, so it never does).
- `security show-keychain-info` on a locked keychain shows an unlock dialog — tests use a timeout.

`scripts/e2e.sh` (macOS section) checks all of it in a sandbox: fake real HOME with its own login keychain, secure + file modes, unlock at spawn after `lock-keychain`, lazy creation for old profiles, items invisible across profiles and from the real HOME, mode switching, `profile rm`, warnings that don't block the shell.

## Same test for railway / gh / vercel
Same steps with `railway login` + `railway whoami`, `gh auth login` + `gh auth status`, `vercel login` + `vercel whoami`.
