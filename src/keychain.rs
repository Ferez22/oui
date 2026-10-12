//! Per-profile macOS keychains.
//!
//! macOS resolves a user's default keychain through `$HOME`
//! (`~/Library/Keychains/login.keychain-db`), so a keychain there automatically
//! becomes the default keychain of every process in that profile. Claude Code,
//! gh, git-credential-osxkeychain and anything else that stores secrets in
//! "the keychain" then isolate per profile with no tool-specific code.
//!
//! The keychain file itself is `oui.keychain-db`, and `login.keychain-db` is a
//! symlink to it: securityd treats a file *named* `login.keychain*` as the
//! user's login keychain and refuses to unlock it with its own password
//! (it validates against the account's keybag), so a profile keychain with
//! that name could never be unlocked again after a reboot.
//!
//! Each profile keychain is protected by a random password that oUI keeps in
//! one of two places, chosen per profile (`keychain = "secure" | "file"`):
//! - `secure` (default): a generic password item in the user's real login
//!   keychain (service [`SERVICE`]). macOS may ask once to let `oui` read it.
//! - `file`: a 0600 file `keychain.pw` in the profile directory. No prompts;
//!   protection equals a plaintext token file.
//!
//! Passwords never go on a command line and are never logged or printed. A
//! stored password is only used if it looks like one oUI made (64 lowercase
//! hex digits), and a new keychain always gets a newly generated password.
//! The keychain file is 0600, in 0700 folders.
//!
//! On other platforms everything here is a no-op: Linux CLIs keep logins in
//! files (isolated by HOME/XDG) or the Secret Service, which is out of scope.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Service name of the password items in the real login keychain.
pub const SERVICE: &str = "oUI profile keychain";
/// Name of the profile keychain file in `<profile home>/Library/Keychains`.
const KEYCHAIN_FILE: &str = "oui.keychain-db";
/// File holding the password in `file` mode, inside the profile directory.
pub const PASSWORD_FILE: &str = "keychain.pw";

/// Where a profile keychain's password is kept.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// In the user's real login keychain (encrypted at rest; macOS may prompt once).
    #[default]
    Secure,
    /// In a 0600 file in the profile directory (no prompts).
    File,
}

impl Mode {
    pub fn label(self) -> &'static str {
        match self {
            Mode::Secure => "secure",
            Mode::File => "file",
        }
    }
}

/// The profile keychain file inside a profile's HOME.
pub fn keychain_path(home: &Path) -> PathBuf {
    home.join("Library/Keychains").join(KEYCHAIN_FILE)
}

/// Where macOS looks for a HOME's default keychain. In a profile: a symlink
/// to [`keychain_path`]; in the real HOME: the user's login keychain.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn login_keychain_path(home: &Path) -> PathBuf {
    home.join("Library/Keychains/login.keychain-db")
}

/// A profile's identity for its password: its name and directory (`<root>/profiles/<name>`).
pub struct Profile<'a> {
    pub name: &'a str,
    pub dir: &'a Path,
}

impl Profile<'_> {
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    fn home(&self) -> PathBuf {
        self.dir.join("home")
    }
}

/// Make sure the profile keychain exists and is unlocked, waiting at most
/// `wait`. Returns a warning to show the user instead of failing: a shell
/// must start even when the keychain can't be prepared. If the work outlives
/// `wait` (e.g. macOS is showing an access prompt), it finishes in the background.
pub fn ensure(p: &Profile, mode: Mode, wait: Duration) -> Option<String> {
    imp::ensure(p, mode, wait)
}

/// Create the keychain now (at `oui profile new`).
pub fn setup(p: &Profile, mode: Mode) -> Result<()> {
    imp::ensure_now(p, mode)
}

/// Move the keychain password from `from` to `to`; keychain contents are kept.
pub fn switch(p: &Profile, from: Mode, to: Mode) -> Result<()> {
    imp::switch(p, from, to)
}

/// Delete the profile's password from wherever it may be stored (before `oui profile rm`).
pub fn forget(p: &Profile) -> Result<()> {
    imp::forget(p)
}

/// Whether the keychain in `home` holds a generic password whose service
/// starts with `prefix`. Attributes only: secret data is never read, and a
/// missing or locked keychain is `false` (never prompts).
pub fn has_service_prefix(home: &Path, prefix: &str) -> bool {
    imp::has_service_prefix(home, prefix)
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::*;

    pub fn ensure(_: &Profile, _: Mode, _: Duration) -> Option<String> {
        None
    }
    pub fn ensure_now(_: &Profile, _: Mode) -> Result<()> {
        Ok(())
    }
    pub fn switch(_: &Profile, _: Mode, _: Mode) -> Result<()> {
        Ok(())
    }
    pub fn forget(_: &Profile) -> Result<()> {
        Ok(())
    }
    pub fn has_service_prefix(_: &Path, _: &str) -> bool {
        false
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::*;
    use crate::profile;
    use anyhow::{Context, anyhow, bail};
    use core_foundation::base::TCFType;
    use security_framework::item::{ItemClass, ItemSearchOptions, Limit};
    use security_framework::os::macos::keychain::{KeychainSettings, SecKeychain};
    use std::collections::HashSet;
    use std::fs;
    use std::io::{Read, Write};
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
    use std::process::{Command, Stdio};
    use std::sync::{Mutex, mpsc};
    use zeroize::Zeroizing;

    type Password = Zeroizing<String>;

    const ERR_ITEM_NOT_FOUND: i32 = -25300; // errSecItemNotFound
    const UNLOCKED: u32 = 1; // kSecUnlockStateStatus

    unsafe extern "C" {
        // Security.framework (linked by security-framework-sys); not wrapped by the crate.
        fn SecKeychainGetStatus(keychain: *const std::ffi::c_void, status: *mut u32) -> i32;
    }

    /// Profiles whose keychain is being prepared (so a pending macOS prompt
    /// isn't stacked with another one for the next pane).
    static PENDING: Mutex<Option<HashSet<PathBuf>>> = Mutex::new(None);

    fn mark_pending(dir: &Path, on: bool) -> bool {
        let mut g = PENDING.lock().unwrap_or_else(|e| e.into_inner());
        let set = g.get_or_insert_with(HashSet::new);
        if on { set.insert(dir.to_path_buf()) } else { set.remove(dir) }
    }

    pub fn ensure(p: &Profile, mode: Mode, wait: Duration) -> Option<String> {
        let (name, dir) = (p.name.to_string(), p.dir.to_path_buf());
        if !mark_pending(&dir, true) {
            return Some(format!("profile '{name}' keychain is still being unlocked (answer the macOS prompt)"));
        }
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let res = ensure_now(&Profile { name: &name, dir: &dir }, mode);
            mark_pending(&dir, false);
            if let Err(Err(e)) = tx.send(res).map_err(|e| e.0) {
                eprintln!("oUI: profile '{name}' keychain: {e:#}");
            }
        });
        match rx.recv_timeout(wait) {
            Ok(Ok(())) => None,
            Ok(Err(e)) => Some(format!(
                "profile '{}' keychain not ready: {e:#}. Logins stored in the macOS keychain won't work in this shell.",
                p.name
            )),
            Err(_) => Some(format!(
                "waiting for macOS to allow oUI to unlock profile '{}' keychain; answer the prompt (Always Allow)",
                p.name
            )),
        }
    }

    pub fn ensure_now(p: &Profile, mode: Mode) -> Result<()> {
        let home = p.home();
        let path = keychain_path(&home);
        let link = login_keychain_path(&home);
        let link_ok = match fs::symlink_metadata(&link) {
            Ok(m) if m.file_type().is_symlink() => {
                if fs::read_link(&link)? != Path::new(KEYCHAIN_FILE) {
                    bail!(
                        "{} doesn't point to {KEYCHAIN_FILE}; move it away so the profile keychain can be used",
                        link.display()
                    );
                }
                true
            }
            Ok(_) => bail!(
                "{} exists and isn't oUI's; move it away so the profile keychain can be used",
                link.display()
            ),
            Err(_) => false,
        };
        if path.exists() {
            profile::tighten(&path, 0o600)?;
            if !link_ok {
                std::os::unix::fs::symlink(KEYCHAIN_FILE, &link)?;
            }
            let mut kc = SecKeychain::open(&path)?;
            if is_unlocked(&kc)? {
                return Ok(());
            }
            let pw = load(p, mode)?.ok_or_else(|| {
                anyhow!(
                    "its password is missing from the {} store; to start over (losing logins kept in it) delete {}",
                    mode.label(),
                    path.display()
                )
            })?;
            kc.unlock(Some(&pw)).context("unlock profile keychain")?;
            kc.set_settings(&no_autolock())?;
            return Ok(());
        }
        // A new keychain always gets a new password (never one already found in
        // a store, which may be stale or not oUI's). Store it before creating
        // the keychain: never a keychain without its password.
        let pw = generate()?;
        store(p, mode, &pw)?;
        create(&home, &path, &pw)?;
        if !link_ok {
            std::os::unix::fs::symlink(KEYCHAIN_FILE, &link)?;
        }
        Ok(())
    }

    pub fn switch(p: &Profile, from: Mode, to: Mode) -> Result<()> {
        if from == to {
            return Ok(());
        }
        match load(p, from)? {
            Some(pw) => {
                store(p, to, &pw)?;
                delete(p, from)?;
            }
            None if keychain_path(&p.home()).exists() => {
                bail!("the profile keychain's password is missing from the {} store", from.label())
            }
            None => {} // created with the new mode at the next shell
        }
        Ok(())
    }

    pub fn forget(p: &Profile) -> Result<()> {
        delete(p, Mode::File)?;
        delete(p, Mode::Secure)
    }

    pub fn has_service_prefix(home: &Path, prefix: &str) -> bool {
        let path = keychain_path(home);
        if !path.exists() {
            return false;
        }
        let Ok(kc) = SecKeychain::open(&path) else { return false };
        if !is_unlocked(&kc).unwrap_or(false) {
            return false;
        }
        // Belt and braces: never show a dialog from here.
        let Ok(_no_ui) = SecKeychain::disable_user_interaction() else { return false };
        let found = ItemSearchOptions::new()
            .keychains(std::slice::from_ref(&kc))
            .class(ItemClass::generic_password())
            .load_attributes(true)
            .limit(Limit::All)
            .search();
        found.map(|items| {
            items.iter().filter_map(|i| i.simplify_dict()).any(|d| d.get("svce").is_some_and(|s| s.starts_with(prefix)))
        })
        .unwrap_or(false)
    }

    fn is_unlocked(kc: &SecKeychain) -> Result<bool> {
        let mut status = 0u32;
        // SAFETY: valid keychain ref for the call; status is a plain out-param.
        let code = unsafe { SecKeychainGetStatus(kc.as_concrete_TypeRef().cast(), &mut status) };
        if code != 0 {
            return Err(security_framework::base::Error::from_code(code)).context("keychain status");
        }
        Ok(status & UNLOCKED != 0)
    }

    fn no_autolock() -> KeychainSettings {
        let mut s = KeychainSettings::new();
        s.set_lock_on_sleep(false);
        s.set_lock_interval(None);
        s
    }

    /// Create the keychain with `security -i`, the password going through its
    /// stdin (never argv). It runs with HOME set to the profile home: creating a
    /// keychain adds it to the *creating HOME's* keychain search list, and it
    /// must never join the real user's list (real-environment lookups would
    /// then see profile secrets).
    fn create(home: &Path, path: &Path, pw: &str) -> Result<()> {
        if let Some(parent) = path.parent() {
            profile::private_dir(parent)?;
        }
        if !is_valid_password(pw) {
            bail!("refusing a malformed profile keychain password");
        }
        let p = path.to_str().context("profile keychain path is not UTF-8")?;
        if p.contains(['"', '\\', '\n']) {
            bail!("profile keychain path can't contain quotes, backslashes or newlines: {p}");
        }
        let mut child = Command::new("/usr/bin/security")
            .arg("-i")
            .env("HOME", home)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .context("run /usr/bin/security")?;
        {
            let mut stdin = child.stdin.take().context("security stdin")?;
            let line = Zeroizing::new(format!("create-keychain -p {pw} \"{p}\"\n"));
            stdin.write_all(line.as_bytes())?;
        } // closing stdin ends the session
        let out = child.wait_with_output()?;
        let err = String::from_utf8_lossy(&out.stderr).replace(pw, "***");
        if !out.status.success() || !path.exists() || !err.trim().is_empty() {
            bail!("create profile keychain: {}", err.trim());
        }
        profile::tighten(path, 0o600)?;
        let mut kc = SecKeychain::open(path)?;
        kc.unlock(Some(pw)).context("unlock new profile keychain")?;
        kc.set_settings(&no_autolock())?;
        Ok(())
    }

    fn generate() -> Result<Password> {
        let mut bytes = Zeroizing::new([0u8; 32]);
        getrandom::fill(bytes.as_mut()).map_err(|e| anyhow!("random password: {e}"))?;
        let mut s = Zeroizing::new(String::with_capacity(64));
        for b in bytes.iter() {
            use std::fmt::Write;
            let _ = write!(s, "{b:02x}");
        }
        Ok(s)
    }

    /// What oUI generates: 64 lowercase hex digits. Anything else is refused
    /// (the password goes into a `security -i` command line).
    fn is_valid_password(pw: &str) -> bool {
        pw.len() == 64 && pw.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    }

    // ---- password stores ----------------------------------------------------

    fn load(p: &Profile, mode: Mode) -> Result<Option<Password>> {
        let pw = match mode {
            Mode::File => {
                let path = p.dir.join(PASSWORD_FILE);
                // Never through a symlink; only a regular file of ours.
                let mut f = match fs::OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                    .open(&path)
                {
                    Ok(f) => f,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                    Err(e) => return Err(e).with_context(|| format!("open {} (it must be a plain file)", path.display())),
                };
                let m = f.metadata()?;
                // SAFETY: geteuid has no preconditions.
                if !m.is_file() || m.uid() != unsafe { libc::geteuid() } {
                    bail!("{} must be a plain file owned by you", path.display());
                }
                let mut s = Zeroizing::new(String::new());
                f.read_to_string(&mut s).context("read keychain password file")?;
                Zeroizing::new(s.trim().to_string())
            }
            Mode::Secure => match real_login()?.find_generic_password(SERVICE, &account(p)) {
                Ok((data, _)) => {
                    let s = std::str::from_utf8(&data).context("keychain password is not UTF-8")?;
                    Zeroizing::new(s.to_string())
                }
                Err(e) if e.code() == ERR_ITEM_NOT_FOUND => return Ok(None),
                Err(e) => return Err(e).context("read profile keychain password from your login keychain"),
            },
        };
        if !is_valid_password(&pw) {
            bail!("the profile keychain password in the {} store isn't one oUI made (64 hex digits)", mode.label());
        }
        Ok(Some(pw))
    }

    fn store(p: &Profile, mode: Mode, pw: &str) -> Result<()> {
        match mode {
            Mode::File => {
                let path = p.dir.join(PASSWORD_FILE);
                let mut f = fs::OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .mode(0o600)
                    .custom_flags(libc::O_NOFOLLOW)
                    .open(&path)
                    .with_context(|| format!("write {} (it must be a plain file)", path.display()))?;
                f.set_permissions(fs::Permissions::from_mode(0o600))?;
                f.write_all(pw.as_bytes())?;
                Ok(())
            }
            Mode::Secure => {
                // Replace without reading the old secret (reading may prompt).
                delete(p, Mode::Secure)?;
                real_login()?
                    .add_generic_password(SERVICE, &account(p), pw.as_bytes())
                    .context("save profile keychain password in your login keychain")
            }
        }
    }

    fn delete(p: &Profile, mode: Mode) -> Result<()> {
        match mode {
            Mode::File => match fs::remove_file(p.dir.join(PASSWORD_FILE)) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
                _ => Ok(()),
            },
            Mode::Secure => {
                let kc = real_login()?;
                let res = ItemSearchOptions::new()
                    .keychains(std::slice::from_ref(&kc))
                    .class(ItemClass::generic_password())
                    .service(SERVICE)
                    .account(&account(p))
                    .delete();
                match res {
                    Err(e) if e.code() != ERR_ITEM_NOT_FOUND => {
                        Err(e).context("delete profile keychain password from your login keychain")
                    }
                    _ => Ok(()),
                }
            }
        }
    }

    /// The real user's login keychain, by explicit path (never via the
    /// process's HOME, which may be a profile's).
    fn real_login() -> Result<SecKeychain> {
        #[cfg(test)]
        bail!("secure mode is disabled in unit tests (it would use the real login keychain)");
        #[cfg(not(test))]
        {
            let path = login_keychain_path(&profile::real_home()?);
            if !path.exists() {
                bail!("login keychain not found at {}", path.display());
            }
            Ok(SecKeychain::open(&path)?)
        }
    }

    /// Account of the password item: the profile name, plus the oUI root
    /// when it isn't the default `~/.oui` (so separate roots never collide).
    fn account(p: &Profile) -> String {
        let default_root = profile::real_home().map(|h| h.join(".oui")).ok();
        match (profile::oui_root().ok(), default_root) {
            (Some(root), Some(def)) if root == def => p.name.to_string(),
            _ => format!("{} ({})", p.name, p.dir.display()),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        struct Tmp(PathBuf);
        impl Drop for Tmp {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        fn tmp(tag: &str) -> Tmp {
            let d = std::env::temp_dir().join(format!("oui-kc-{tag}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&d);
            fs::create_dir_all(d.join("home")).unwrap();
            Tmp(d)
        }

        #[test]
        fn generated_passwords_are_random_hex() {
            let (a, b) = (generate().unwrap(), generate().unwrap());
            assert_eq!(a.len(), 64);
            assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
            assert_ne!(*a, *b);
        }

        #[test]
        fn only_generated_looking_passwords_are_accepted() {
            assert!(is_valid_password(&generate().unwrap()));
            assert!(!is_valid_password(""));
            assert!(!is_valid_password(&"A".repeat(64)));
            assert!(!is_valid_password(&format!("{}\" \"/tmp/x", "a".repeat(58))));
            assert!(!is_valid_password(&"a".repeat(65)));
        }

        #[test]
        fn file_password_is_validated_and_never_read_through_a_symlink() {
            let t = tmp("pwfile");
            let p = Profile { name: "t", dir: &t.0 };
            let path = t.0.join(PASSWORD_FILE);
            fs::write(&path, "x\" \"; rm -rf /tmp/never\n").unwrap();
            assert!(load(&p, Mode::File).is_err());
            let good = "ab".repeat(32);
            fs::write(&path, format!("{good}\n")).unwrap();
            assert_eq!(load(&p, Mode::File).unwrap().unwrap().as_str(), good);
            fs::remove_file(&path).unwrap();
            let target = t.0.join("elsewhere");
            fs::write(&target, &good).unwrap();
            std::os::unix::fs::symlink(&target, &path).unwrap();
            assert!(load(&p, Mode::File).is_err(), "load follows a symlink");
            assert!(store(&p, Mode::File, &good).is_err(), "store follows a symlink");
        }

        #[test]
        fn foreign_login_keychain_link_is_refused() {
            let t = tmp("link");
            let p = Profile { name: "t", dir: &t.0 };
            let kc = p.home().join("Library/Keychains");
            fs::create_dir_all(&kc).unwrap();
            std::os::unix::fs::symlink("/tmp/not-oui.keychain-db", kc.join("login.keychain-db")).unwrap();
            let e = ensure_now(&p, Mode::File).unwrap_err();
            assert!(format!("{e:#}").contains("doesn't point to oui.keychain-db"), "{e:#}");
            assert!(!t.0.join(PASSWORD_FILE).exists());
        }

        #[test]
        fn secure_mode_never_touches_the_real_keychain_in_tests() {
            let t = tmp("secure");
            let p = Profile { name: "t", dir: &t.0 };
            assert!(ensure_now(&p, Mode::Secure).is_err());
            assert!(!keychain_path(&p.home()).exists());
        }
    }
}
