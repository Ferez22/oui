//! Profiles: each profile has its own HOME/XDG dirs so every CLI tool keeps a
//! separate login per profile. Every process oUI spawns goes through `spawn_env`.

use crate::keychain;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::fs::{DirBuilder, File, OpenOptions, Permissions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;
use std::{env, fs, io};

/// Name shown for tabs that run in the user's normal environment (no isolation).
pub const SYSTEM: &str = "system";

/// Files and folders linked from the real HOME into a profile when
/// `shared = true`, so shells, prompts and toolchains keep working. Nothing
/// here stores logins: credential locations (~/.railway, ~/.config/gh,
/// ~/.claude, ~/.cargo/credentials.toml, ~/.ssh, ~/.npmrc…) are never linked.
/// Caveat: these are still *your* files. A token exported from a shared rc
/// file, or a fish universal variable (`set -Ux`, kept in
/// .config/fish/fish_variables), reaches every shared profile; keep tokens out
/// of shell config or in the profile's own files.
const SHARED_ALLOWLIST: &[&str] = &[
    ".zshrc", ".zprofile", ".zshenv", ".zlogin", ".bashrc", ".bash_profile", ".profile",
    ".inputrc", ".vimrc", ".vim", ".nanorc", ".tmux.conf", ".gitignore_global",
    // shell frameworks and prompts
    ".oh-my-zsh", ".p10k.zsh", ".zsh", ".zinit", ".antidote", ".zsh_plugins.txt", ".zplug",
    ".fzf", ".fzf.zsh", ".fzf.bash", ".config/fish", ".config/zsh", ".config/starship.toml",
    // toolchains and version managers (.cargo: see SHARED_PARTS)
    ".nvm", ".volta", ".rustup", ".bun", ".deno", ".pyenv", ".rbenv", ".asdf",
    ".sdkman", ".config/mise", ".local/share/mise", ".local/bin",
    // editors
    ".config/nvim", ".config/helix",
];

/// Folders that mix toolchain files with credentials. A shared profile gets
/// its own real folder holding links to only the listed credential-free parts,
/// so a login there (`cargo login` → `.cargo/credentials.toml`) stays in the
/// profile. Cargo's config.toml (may hold registry tokens) and its
/// registry/git caches (guarded by a lock file in the cargo home itself, so
/// two cargo homes sharing them could corrupt them) are not linked.
const SHARED_PARTS: &[(&str, &[&str])] = &[(".cargo", &["bin", "env", "env.fish", "env.nu"])];

/// Variables that would override per-profile logins: tokens, and paths that
/// point tools back at the real HOME's credential files. See also
/// [`is_credential_var`] for the prefix and case-insensitive rules.
const CREDENTIAL_VARS: &[&str] = &[
    "ANTHROPIC_API_KEY", "CLAUDE_CODE_OAUTH_TOKEN", "RAILWAY_TOKEN", "RAILWAY_API_TOKEN",
    "GH_TOKEN", "GITHUB_TOKEN", "VERCEL_TOKEN", "NPM_TOKEN", "FLY_ACCESS_TOKEN", "FLY_API_TOKEN",
    "SUPABASE_ACCESS_TOKEN", "STRIPE_API_KEY", "NETLIFY_AUTH_TOKEN",
    "AWS_ACCESS_KEY_ID", "AWS_SECRET_ACCESS_KEY", "AWS_SESSION_TOKEN", "AWS_PROFILE",
    "AWS_SHARED_CREDENTIALS_FILE", "AWS_CONFIG_FILE", "DOCKER_CONFIG",
    "NPM_CONFIG_USERCONFIG", "GIT_CONFIG_GLOBAL",
    "CARGO_HOME", "CARGO_REGISTRY_TOKEN", "KUBECONFIG", "GOOGLE_APPLICATION_CREDENTIALS",
    "CLOUDSDK_CONFIG", "AZURE_CONFIG_DIR",
];

/// Whether `name` is a variable to remove in profiles: [`CREDENTIAL_VARS`],
/// `CARGO_REGISTRIES_<name>_TOKEN`, and npm settings (`npm_config_*`, any
/// case, as npm reads them) for auth tokens or other npmrc files.
pub fn is_credential_var(name: &str) -> bool {
    if CREDENTIAL_VARS.contains(&name) {
        return true;
    }
    if name.starts_with("CARGO_REGISTRIES_") && name.ends_with("_TOKEN") {
        return true;
    }
    let lower = name.to_ascii_lowercase();
    lower.starts_with("npm_config_")
        && ["auth", "token", "userconfig", "globalconfig"].iter().any(|k| lower.contains(k))
}

/// Credential variables to remove: the fixed list plus every matching
/// variable among `present` (the current environment's names).
fn credential_vars(present: impl Iterator<Item = String>) -> Vec<String> {
    let mut out: Vec<String> = CREDENTIAL_VARS.iter().map(|s| s.to_string()).collect();
    for name in present {
        if is_credential_var(&name) && !out.contains(&name) {
            out.push(name);
        }
    }
    out
}

#[derive(Serialize, Deserialize, Clone)]
pub struct ProfileConfig {
    pub name: String,
    pub color: String,
    pub shared: bool,
    /// Extra paths (relative to HOME) to share on top of the built-in allowlist.
    #[serde(default)]
    pub extra_shared: Vec<String>,
    /// Where the profile keychain's password is kept (macOS). Profiles created
    /// before this setting existed are `secure`; their keychain is created at
    /// the first shell.
    #[serde(default)]
    pub keychain: keychain::Mode,
}

/// The oUI root (`$OUI_HOME`, else `~/.oui`). Refused when it exists but
/// belongs to another user: it holds every profile's logins and the socket.
pub fn oui_root() -> Result<PathBuf> {
    let root = match env::var("OUI_HOME") {
        Ok(p) => PathBuf::from(p),
        Err(_) => real_home()?.join(".oui"),
    };
    if let Ok(m) = fs::metadata(&root) {
        let me = euid();
        if m.uid() != me {
            bail!(
                "{} belongs to another user (uid {}, you are {me}); oUI keeps your logins there, so it must be yours. \
                 Set OUI_HOME to a folder you own.",
                root.display(),
                m.uid()
            );
        }
    }
    Ok(root)
}

fn euid() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

/// Create the oUI root (0700) if needed and tighten an existing one.
pub fn ensure_root() -> Result<PathBuf> {
    let root = oui_root()?;
    private_dir(&root)?;
    oui_root() // re-check ownership now that it exists
}

/// Create `path` and missing parents with mode 0700; an existing `path` of
/// ours that's more open is tightened to 0700 (symlinks are left alone).
pub fn private_dir(path: &Path) -> Result<()> {
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .with_context(|| format!("create {}", path.display()))?;
    tighten(path, 0o700)
}

/// Remove group/other permissions beyond `mode` from `path` if it's ours.
/// Never follows a symlink; a missing path or a symlink is left alone.
pub fn tighten(path: &Path, mode: u32) -> Result<()> {
    let f = match File::options()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound || e.raw_os_error() == Some(libc::ELOOP) => return Ok(()),
        Err(e) => return Err(e).with_context(|| format!("open {}", path.display())),
    };
    let m = f.metadata()?;
    if m.uid() == euid() && m.mode() & 0o777 & !mode != 0 {
        f.set_permissions(Permissions::from_mode(mode))
            .with_context(|| format!("chmod {:o} {}", mode, path.display()))?;
    }
    Ok(())
}

/// Write `contents` to `path` with mode 0600 (an existing file is tightened too).
pub fn write_private(path: &Path, contents: &[u8]) -> Result<()> {
    let mut f = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("write {}", path.display()))?;
    f.set_permissions(Permissions::from_mode(0o600))?;
    f.write_all(contents)?;
    Ok(())
}

/// Make sure the directories above and around a profile's logins are private
/// (0700): the oUI root, profiles/, the profile, its HOME and, on macOS, the
/// HOME's Library and Library/Keychains (0700 in a real macOS HOME too).
fn ensure_private_tree(dir: &Path) -> Result<()> {
    let root = ensure_root()?;
    private_dir(&root.join("profiles"))?;
    private_dir(dir)?;
    tighten(&dir.join("profile.toml"), 0o600)?;
    let home = dir.join("home");
    private_dir(&home)?;
    if cfg!(target_os = "macos") {
        private_dir(&home.join("Library/Keychains"))?;
        tighten(&home.join("Library"), 0o700)?;
    }
    Ok(())
}

/// The user's real home, even when called from inside a profile.
pub fn real_home() -> Result<PathBuf> {
    env::var("OUI_REAL_HOME")
        .or_else(|_| env::var("HOME"))
        .map(PathBuf::from)
        .context("HOME not set")
}

pub fn profile_dir(name: &str) -> Result<PathBuf> {
    if name == SYSTEM {
        bail!("'{SYSTEM}' is reserved for your normal environment");
    }
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        bail!("profile name must be letters, digits, '-' or '_'");
    }
    Ok(oui_root()?.join("profiles").join(name))
}

pub fn load(name: &str) -> Result<(PathBuf, ProfileConfig)> {
    let dir = profile_dir(name)?;
    let raw = fs::read_to_string(dir.join("profile.toml"))
        .with_context(|| format!("profile '{name}' not found (create it: oui profile new {name})"))?;
    Ok((dir, toml::from_str(&raw)?))
}

pub fn save(dir: &Path, cfg: &ProfileConfig) -> Result<()> {
    write_private(&dir.join("profile.toml"), toml::to_string_pretty(cfg)?.as_bytes())
}

pub fn list() -> Vec<ProfileConfig> {
    let Ok(root) = oui_root() else { return vec![] };
    let mut out: Vec<_> = fs::read_dir(root.join("profiles"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| load(&e.file_name().to_string_lossy()).ok().map(|(_, c)| c))
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Create a profile. A keychain setup failure is returned as a warning (the
/// keychain is retried when a shell starts), not an error.
pub fn create(name: &str, color: &str, fresh: bool, mode: keychain::Mode) -> Result<(PathBuf, Option<String>)> {
    let dir = profile_dir(name)?;
    if dir.exists() {
        bail!("profile '{name}' already exists");
    }
    ensure_private_tree(&dir)?;
    let home = dir.join("home");
    for sub in [".config", ".local/share", ".local/state", ".cache", ".claude"] {
        private_dir(&home.join(sub))?;
    }
    let cfg = ProfileConfig { name: name.into(), color: color.into(), shared: !fresh, extra_shared: vec![], keychain: mode };
    save(&dir, &cfg)?;
    sync_shared(&home, &cfg)?;
    let warning = keychain::setup(&keychain::Profile { name, dir: &dir }, mode)
        .err()
        .map(|e| format!("couldn't create the profile keychain yet ({e:#}); it will be retried when a shell starts"));
    Ok((dir, warning))
}

/// Switch where the profile keychain's password is kept; keychain contents are kept.
pub fn set_keychain_mode(name: &str, mode: keychain::Mode) -> Result<()> {
    let (dir, mut cfg) = load(name)?;
    if cfg.keychain == mode {
        return Ok(());
    }
    keychain::switch(&keychain::Profile { name, dir: &dir }, cfg.keychain, mode)?;
    cfg.keychain = mode;
    save(&dir, &cfg)
}

/// Delete a profile: its directory (HOME, logins, keychain) and its keychain
/// password. Callers must confirm with the user first.
pub fn remove(name: &str) -> Result<Option<String>> {
    let (dir, _) = load(name)?;
    let warning = keychain::forget(&keychain::Profile { name, dir: &dir })
        .err()
        .map(|e| format!("couldn't remove the keychain password item '{}' ({e:#}); delete it in Keychain Access", keychain::SERVICE));
    // remove_dir_all unlinks symlinks (shared dotfiles) without following them.
    fs::remove_dir_all(&dir).with_context(|| format!("delete {}", dir.display()))?;
    Ok(warning)
}

pub fn set_shared(name: &str, on: bool) -> Result<()> {
    let (dir, mut cfg) = load(name)?;
    cfg.shared = on;
    save(&dir, &cfg)?;
    sync_shared(&dir.join("home"), &cfg)
}

/// Link (or unlink) allowlisted dotfiles. Only removes entries that are our symlinks.
fn sync_shared(home: &Path, cfg: &ProfileConfig) -> Result<()> {
    let real = real_home()?;
    let shared = cfg.shared;
    for rel in &cfg.extra_shared {
        check_extra_shared(rel)?;
    }
    for (rel, parts) in SHARED_PARTS {
        sync_parts(&real, home, rel, parts, shared)?;
    }
    let extra = cfg.extra_shared.iter().map(String::as_str);
    for rel in SHARED_ALLOWLIST.iter().copied().chain(extra) {
        link_or_unlink(&real.join(rel), &home.join(rel), shared)?;
    }
    Ok(())
}

fn is_symlink(p: &Path) -> bool {
    p.symlink_metadata().map(|m| m.file_type().is_symlink()).unwrap_or(false)
}

/// Shared: link `dst` to `src` if `src` exists and `dst` is free. Unshared:
/// remove `dst` if it is a symlink (only oUI makes symlinks there).
fn link_or_unlink(src: &Path, dst: &Path, shared: bool) -> Result<()> {
    let is_link = is_symlink(dst);
    if shared {
        if src.exists() && !dst.exists() && !is_link {
            if let Some(parent) = dst.parent() {
                fs::create_dir_all(parent)?;
            }
            std::os::unix::fs::symlink(src, dst)?;
        }
    } else if is_link {
        fs::remove_file(dst)?;
    }
    Ok(())
}

/// A [`SHARED_PARTS`] folder: the profile's own `home/<rel>` (0700) with
/// links to the real folder's credential-free `parts`.
fn sync_parts(real: &Path, home: &Path, rel: &str, parts: &[&str], shared: bool) -> Result<()> {
    let src = real.join(rel);
    let dst = home.join(rel);
    // Profiles made by older versions link the whole real folder (and with it
    // the real credentials). Remove that link itself; its target is untouched.
    if is_symlink(&dst) {
        let ours = fs::read_link(&dst).is_ok_and(|t| t == src)
            || matches!((fs::canonicalize(&dst), fs::canonicalize(&src)), (Ok(a), Ok(b)) if a == b);
        if !ours {
            return Ok(()); // the user's own link: leave it
        }
        fs::remove_file(&dst).with_context(|| format!("remove old link {}", dst.display()))?;
    }
    if shared {
        if !src.is_dir() {
            return Ok(());
        }
        private_dir(&dst)?;
    } else if !dst.is_dir() {
        return Ok(());
    }
    for part in parts {
        link_or_unlink(&src.join(part), &dst.join(part), shared)?;
    }
    Ok(())
}

/// `extra_shared` entries must stay inside HOME and away from the profile keychain.
fn check_extra_shared(rel: &str) -> Result<()> {
    let p = Path::new(rel);
    if rel.is_empty() || !p.components().all(|c| matches!(c, Component::Normal(_))) {
        bail!("extra_shared entry {rel:?}: use a path relative to HOME, without '.' or '..' (e.g. \".config/foo\")");
    }
    // Case-insensitive: macOS file systems usually are.
    let lower = PathBuf::from(rel.to_ascii_lowercase());
    let keychains = Path::new("library/keychains");
    if lower.starts_with(keychains) || keychains.starts_with(&lower) {
        bail!("extra_shared entry {rel:?}: Library/Keychains holds the profile's own keychain and can't be shared");
    }
    Ok(())
}

// ---- per-profile ssh -------------------------------------------------------

/// First line after the shebang of the generated ssh wrapper.
const SSH_WRAPPER_MARK: &str = "# oUI ssh wrapper";

/// Folder put first on PATH in a profile that has its own `.ssh/config`.
fn profile_bin(dir: &Path) -> PathBuf {
    dir.join("bin")
}

fn is_ssh_wrapper(path: &Path) -> bool {
    let mut head = [0u8; 64];
    let n = File::options()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .and_then(|mut f| io::Read::read(&mut f, &mut head))
        .unwrap_or(0);
    String::from_utf8_lossy(&head[..n]).lines().nth(1).is_some_and(|l| l.starts_with(SSH_WRAPPER_MARK))
}

/// Single-quote `s` for sh.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// The real ssh: the first `ssh` on PATH that isn't an oUI wrapper, else /usr/bin/ssh.
fn real_ssh(path_var: &str, profiles: &Path) -> PathBuf {
    path_var
        .split(':')
        .filter(|d| !d.is_empty() && !Path::new(d).starts_with(profiles))
        .map(|d| Path::new(d).join("ssh"))
        .find(|p| {
            fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0) && !is_ssh_wrapper(p)
        })
        .unwrap_or_else(|| PathBuf::from("/usr/bin/ssh"))
}

/// ssh ignores HOME (it reads the passwd entry), so a profile's own
/// `.ssh/config` needs `-F`: when it exists, write `<profile>/bin/ssh`, which
/// runs the real ssh with it (`env_for` puts it first on PATH and in
/// GIT_SSH_COMMAND). Without one, ssh is left alone and the wrapper removed.
fn sync_ssh_wrapper(dir: &Path, name: &str) -> Result<()> {
    let bin = profile_bin(dir);
    let wrapper = bin.join("ssh");
    let config = dir.join("home/.ssh/config");
    if !config.is_file() {
        if is_ssh_wrapper(&wrapper) {
            fs::remove_file(&wrapper)?;
        }
        return Ok(());
    }
    let profiles = oui_root()?.join("profiles");
    let real = real_ssh(&env::var("PATH").unwrap_or_default(), &profiles);
    let (Some(real), Some(config)) = (real.to_str(), config.to_str()) else {
        bail!("ssh or the profile's .ssh/config has a non UTF-8 path");
    };
    let script = format!(
        "#!/bin/sh\n{SSH_WRAPPER_MARK} for profile '{name}' (rewritten at every shell start): its own ssh config.\n\
         exec {} -F {} \"$@\"\n",
        sh_quote(real),
        sh_quote(config)
    );
    if fs::read_to_string(&wrapper).is_ok_and(|s| s == script) {
        return tighten(&wrapper, 0o700);
    }
    private_dir(&bin)?;
    let tmp = bin.join(format!(".ssh.{}.tmp", std::process::id()));
    let _ = fs::remove_file(&tmp);
    let mut f = OpenOptions::new().write(true).create_new(true).mode(0o700).open(&tmp)?;
    f.write_all(script.as_bytes())?;
    drop(f);
    fs::rename(&tmp, &wrapper)?;
    Ok(())
}

/// PATH without other profiles' wrapper folders (e.g. inherited when oUI was
/// started from a profile shell), `bin` first when given.
fn profile_path(path_var: &str, profiles: &Path, bin: Option<&Path>) -> String {
    let rest = path_var.split(':').filter(|d| d.is_empty() || !Path::new(d).starts_with(profiles));
    bin.map(|b| b.to_string_lossy().into_owned()).into_iter().chain(rest.map(String::from)).collect::<Vec<_>>().join(":")
}

/// Environment for a process in `profile` (`None` = system): variables to set
/// and variables to remove. Tools that honour HOME/XDG isolate automatically;
/// tool-specific vars cover the rest.
pub struct SpawnEnv {
    pub set: Vec<(String, String)>,
    pub remove: Vec<String>,
    pub color: String,
    /// Something the user should see in the new shell (e.g. the profile
    /// keychain couldn't be unlocked). The process still starts.
    pub warning: Option<String>,
}

/// How long a pane spawn waits for the profile keychain before starting the
/// shell anyway (normally milliseconds; longer only while macOS shows a prompt).
pub const SPAWN_KEYCHAIN_WAIT: Duration = Duration::from_secs(3);

/// Prepare and describe the environment of a new process in `profile`:
/// refresh shared dotfile links and make sure the profile keychain is unlocked.
pub fn spawn_env(profile: Option<&str>) -> Result<SpawnEnv> {
    spawn_env_with(profile, Some(SPAWN_KEYCHAIN_WAIT))
}

/// `spawn_env` with an explicit keychain wait (`None`: leave the keychain alone).
pub fn spawn_env_with(profile: Option<&str>, keychain_wait: Option<Duration>) -> Result<SpawnEnv> {
    let mut warning = None;
    if let Some(name) = profile {
        let (dir, cfg) = load(name)?;
        ensure_private_tree(&dir)?;
        sync_shared(&dir.join("home"), &cfg)?;
        sync_ssh_wrapper(&dir, name)?;
        if let Some(wait) = keychain_wait {
            warning = keychain::ensure(&keychain::Profile { name, dir: &dir }, cfg.keychain, wait);
        }
    }
    Ok(SpawnEnv { warning, ..env_for(profile)? })
}

/// Like `spawn_env` but read-only: doesn't refresh shared dotfile links.
pub fn env_for(profile: Option<&str>) -> Result<SpawnEnv> {
    // Our pane emulator speaks xterm; a TERM every server knows also keeps
    // nano/vim working over ssh.
    let mut set = vec![("TERM".to_string(), "xterm-256color".to_string())];
    let profiles = oui_root()?.join("profiles");
    let path_var = env::var("PATH").ok();
    // A GIT_SSH_COMMAND pointing at some profile's wrapper (inherited) must not leak.
    let mut remove = vec!["OUI_PROFILE_BIN".to_string()];
    if env::var("GIT_SSH_COMMAND").is_ok_and(|c| c.contains(&*profiles.to_string_lossy())) {
        remove.push("GIT_SSH_COMMAND".into());
    }
    let Some(name) = profile else {
        set.push(("OUI_PROFILE".into(), SYSTEM.into()));
        if let Some(p) = &path_var {
            set.push(("PATH".into(), profile_path(p, &profiles, None)));
        }
        return Ok(SpawnEnv { set, remove, color: "gray".into(), warning: None });
    };
    let (dir, cfg) = load(name)?;
    let home = dir.join("home");
    let h = |rel: &str| home.join(rel).to_string_lossy().into_owned();
    set.extend([
        ("OUI_PROFILE".into(), name.into()),
        ("OUI_PROFILE_COLOR".into(), cfg.color.clone()),
        ("OUI_REAL_HOME".into(), real_home()?.to_string_lossy().into_owned()),
        ("HOME".into(), home.to_string_lossy().into_owned()),
        ("XDG_CONFIG_HOME".into(), h(".config")),
        ("XDG_DATA_HOME".into(), h(".local/share")),
        ("XDG_STATE_HOME".into(), h(".local/state")),
        ("XDG_CACHE_HOME".into(), h(".cache")),
        ("CLAUDE_CONFIG_DIR".into(), h(".claude")),
        ("GH_CONFIG_DIR".into(), h(".config/gh")),
    ]);
    let bin = profile_bin(&dir);
    let wrapper = bin.join("ssh");
    let ssh = home.join(".ssh/config").is_file() && is_ssh_wrapper(&wrapper);
    if let Some(p) = &path_var {
        set.push(("PATH".into(), profile_path(p, &profiles, ssh.then_some(bin.as_path()))));
    }
    if ssh {
        remove.retain(|v| v != "OUI_PROFILE_BIN" && v != "GIT_SSH_COMMAND");
        set.push(("OUI_PROFILE_BIN".into(), bin.to_string_lossy().into_owned()));
        set.push(("GIT_SSH_COMMAND".into(), sh_quote(&wrapper.to_string_lossy())));
    }
    remove.extend(credential_vars(env::vars_os().filter_map(|(k, _)| k.into_string().ok())));
    Ok(SpawnEnv { set, remove, color: cfg.color, warning: None })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_without_keychain_setting_are_secure() {
        let cfg: ProfileConfig = toml::from_str("name = \"work\"\ncolor = \"green\"\nshared = true\n").unwrap();
        assert_eq!(cfg.keychain, keychain::Mode::Secure);
        let cfg: ProfileConfig =
            toml::from_str("name = \"w\"\ncolor = \"g\"\nshared = true\nkeychain = \"file\"\n").unwrap();
        assert_eq!(cfg.keychain, keychain::Mode::File);
        assert!(toml::to_string(&cfg).unwrap().contains("keychain = \"file\""));
        assert!(toml::from_str::<ProfileConfig>("name = \"w\"\ncolor = \"g\"\nshared = true\nkeychain = \"x\"\n").is_err());
    }

    struct Tmp(PathBuf);
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn tmp(tag: &str) -> Tmp {
        let d = env::temp_dir().join(format!("oui-prof-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        Tmp(d)
    }
    fn mode(p: &Path) -> u32 {
        fs::symlink_metadata(p).unwrap().mode() & 0o777
    }

    #[test]
    fn credential_vars_match_names_prefixes_and_npm_settings() {
        for v in ["CARGO_REGISTRY_TOKEN", "CARGO_HOME", "FLY_API_TOKEN", "NPM_TOKEN", "KUBECONFIG",
                  "GOOGLE_APPLICATION_CREDENTIALS", "CLOUDSDK_CONFIG", "AZURE_CONFIG_DIR",
                  "CARGO_REGISTRIES_MY_CORP_TOKEN", "npm_config__authToken", "NPM_CONFIG__AUTH",
                  "npm_config_//registry.npmjs.org/:_authToken", "npm_config_userconfig", "NPM_CONFIG_USERCONFIG"] {
            assert!(is_credential_var(v), "{v}");
        }
        for v in ["PATH", "HOME", "CARGO_TERM_COLOR", "CARGO_REGISTRIES_X_INDEX", "npm_config_registry", "TOKEN"] {
            assert!(!is_credential_var(v), "{v}");
        }
        let got = credential_vars(["PATH", "CARGO_REGISTRIES_A_TOKEN", "npm_config__auth"].map(String::from).into_iter());
        assert!(got.contains(&"CARGO_REGISTRIES_A_TOKEN".into()) && got.contains(&"npm_config__auth".into()));
        assert!(got.contains(&"CARGO_HOME".into()) && !got.contains(&"PATH".into()));
    }

    #[test]
    fn extra_shared_stays_inside_home_and_out_of_the_keychains() {
        for ok in [".config/foo", "bin", ".foo/bar.toml", "Library/Fonts"] {
            check_extra_shared(ok).unwrap();
        }
        for bad in ["", "/etc", "../x", ".config/../../x", "./x", "Library", "Library/Keychains",
                    "library/keychains/login.keychain-db", "Library/Keychains/"] {
            assert!(check_extra_shared(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn cargo_gets_its_own_folder_with_only_toolchain_links() {
        let t = tmp("cargo");
        let (real, home) = (t.0.join("real"), t.0.join("home"));
        for d in [real.join(".cargo/bin"), real.join(".cargo/registry"), home.clone()] {
            fs::create_dir_all(d).unwrap();
        }
        fs::write(real.join(".cargo/env"), "env").unwrap();
        fs::write(real.join(".cargo/credentials.toml"), "dummy-real").unwrap();
        fs::write(real.join(".cargo/config.toml"), "x").unwrap();
        let parts = SHARED_PARTS[0].1;
        // A profile made by an older version: home/.cargo -> real ~/.cargo.
        std::os::unix::fs::symlink(real.join(".cargo"), home.join(".cargo")).unwrap();
        sync_parts(&real, &home, ".cargo", parts, true).unwrap();
        let c = home.join(".cargo");
        assert!(c.is_dir() && !is_symlink(&c));
        assert_eq!(mode(&c), 0o700);
        assert_eq!(fs::read_link(c.join("bin")).unwrap(), real.join(".cargo/bin"));
        assert!(is_symlink(&c.join("env")));
        for absent in ["credentials.toml", "config.toml", "registry"] {
            assert!(!c.join(absent).exists(), "{absent}");
        }
        assert_eq!(fs::read_to_string(real.join(".cargo/credentials.toml")).unwrap(), "dummy-real");
        fs::write(c.join("credentials.toml"), "dummy-p").unwrap();
        assert_eq!(fs::read_to_string(real.join(".cargo/credentials.toml")).unwrap(), "dummy-real");
        // Unsharing removes the links, keeps the profile's own login.
        sync_parts(&real, &home, ".cargo", parts, false).unwrap();
        assert!(!c.join("bin").exists() && !is_symlink(&c.join("bin")));
        assert_eq!(fs::read_to_string(c.join("credentials.toml")).unwrap(), "dummy-p");
        // A user's own link elsewhere is left alone.
        let other = t.0.join("other");
        fs::create_dir_all(&other).unwrap();
        let h2 = t.0.join("home2");
        fs::create_dir_all(&h2).unwrap();
        std::os::unix::fs::symlink(&other, h2.join(".cargo")).unwrap();
        sync_parts(&real, &h2, ".cargo", parts, true).unwrap();
        assert_eq!(fs::read_link(h2.join(".cargo")).unwrap(), other);
    }

    #[test]
    fn private_dirs_are_0700_and_tightening_never_follows_symlinks() {
        let t = tmp("modes");
        let d = t.0.join("a/b/c");
        private_dir(&d).unwrap();
        assert_eq!((mode(&t.0.join("a")), mode(&d)), (0o700, 0o700));
        fs::set_permissions(&d, Permissions::from_mode(0o755)).unwrap();
        private_dir(&d).unwrap();
        assert_eq!(mode(&d), 0o700);
        let target = t.0.join("open");
        fs::create_dir(&target).unwrap();
        fs::set_permissions(&target, Permissions::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink(&target, t.0.join("link")).unwrap();
        tighten(&t.0.join("link"), 0o700).unwrap();
        assert_eq!(mode(&target), 0o755);
        let f = t.0.join("f");
        fs::write(&f, "x").unwrap();
        fs::set_permissions(&f, Permissions::from_mode(0o644)).unwrap();
        write_private(&f, b"y").unwrap();
        assert_eq!(mode(&f), 0o600);
        write_private(&t.0.join("g"), b"y").unwrap();
        assert_eq!(mode(&t.0.join("g")), 0o600);
    }

    #[test]
    fn ssh_wrapper_path_and_quoting() {
        assert_eq!(sh_quote("/a b/it's"), r"'/a b/it'\''s'");
        let profiles = Path::new("/r/profiles");
        assert_eq!(profile_path("/r/profiles/a/bin:/usr/bin::/bin", profiles, Some(Path::new("/r/profiles/b/bin"))),
                   "/r/profiles/b/bin:/usr/bin::/bin");
        assert_eq!(profile_path("/r/profiles/a/bin:/usr/bin", profiles, None), "/usr/bin");
        let t = tmp("ssh");
        let (w, fake) = (t.0.join("profiles/p/bin"), t.0.join("fake"));
        for d in [&w, &fake] {
            fs::create_dir_all(d).unwrap();
        }
        for (dir, body) in [(&w, format!("#!/bin/sh\n{SSH_WRAPPER_MARK} x\n")), (&fake, "#!/bin/sh\n".into())] {
            fs::write(dir.join("ssh"), body).unwrap();
            fs::set_permissions(dir.join("ssh"), Permissions::from_mode(0o755)).unwrap();
        }
        let path = format!("{}:{}", w.display(), fake.display());
        assert_eq!(real_ssh(&path, &t.0.join("profiles")), fake.join("ssh"));
        // A wrapper outside the profiles folder is still never picked.
        assert_eq!(real_ssh(&w.display().to_string(), Path::new("/elsewhere")), PathBuf::from("/usr/bin/ssh"));
    }
}
