//! Profiles: each profile has its own HOME/XDG dirs so every CLI tool keeps a
//! separate login per profile. Every process oUI spawns goes through `spawn_env`.

use crate::keychain;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;
use std::{env, fs};

/// Name shown for tabs that run in the user's normal environment (no isolation).
pub const SYSTEM: &str = "system";

/// Non-credential files and folders linked from the real HOME into a profile
/// when `shared = true`, so shells, prompts and toolchains keep working.
/// Credential locations (e.g. ~/.railway, ~/.config/gh, ~/.claude) are never linked.
const SHARED_ALLOWLIST: &[&str] = &[
    ".zshrc", ".zprofile", ".zshenv", ".zlogin", ".bashrc", ".bash_profile", ".profile",
    ".inputrc", ".vimrc", ".vim", ".nanorc", ".tmux.conf", ".gitignore_global",
    // shell frameworks and prompts
    ".oh-my-zsh", ".p10k.zsh", ".zsh", ".zinit", ".antidote", ".zsh_plugins.txt", ".zplug",
    ".fzf", ".fzf.zsh", ".fzf.bash", ".config/fish", ".config/zsh", ".config/starship.toml",
    // toolchains and version managers
    ".nvm", ".volta", ".cargo", ".rustup", ".bun", ".deno", ".pyenv", ".rbenv", ".asdf",
    ".sdkman", ".config/mise", ".local/share/mise", ".local/bin",
    // editors
    ".config/nvim", ".config/helix",
];

/// Variables that would override per-profile logins: tokens, and paths that
/// point tools back at the real HOME's credential files.
const CREDENTIAL_VARS: &[&str] = &[
    "ANTHROPIC_API_KEY", "CLAUDE_CODE_OAUTH_TOKEN", "RAILWAY_TOKEN", "RAILWAY_API_TOKEN",
    "GH_TOKEN", "GITHUB_TOKEN", "VERCEL_TOKEN", "NPM_TOKEN", "FLY_ACCESS_TOKEN",
    "SUPABASE_ACCESS_TOKEN", "STRIPE_API_KEY", "NETLIFY_AUTH_TOKEN",
    "AWS_ACCESS_KEY_ID", "AWS_SECRET_ACCESS_KEY", "AWS_SESSION_TOKEN", "AWS_PROFILE",
    "AWS_SHARED_CREDENTIALS_FILE", "AWS_CONFIG_FILE", "DOCKER_CONFIG",
    "NPM_CONFIG_USERCONFIG", "GIT_CONFIG_GLOBAL",
];

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

pub fn oui_root() -> Result<PathBuf> {
    if let Ok(p) = env::var("OUI_HOME") {
        return Ok(PathBuf::from(p));
    }
    Ok(real_home()?.join(".oui"))
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
    fs::write(dir.join("profile.toml"), toml::to_string_pretty(cfg)?)?;
    Ok(())
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
    let home = dir.join("home");
    for sub in [".config", ".local/share", ".local/state", ".cache", ".claude"] {
        fs::create_dir_all(home.join(sub))?;
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
    let extra = cfg.extra_shared.iter().map(String::as_str);
    for rel in SHARED_ALLOWLIST.iter().copied().chain(extra) {
        let src = real.join(rel);
        let dst = home.join(rel);
        let is_link = dst.symlink_metadata().map(|m| m.file_type().is_symlink()).unwrap_or(false);
        if shared {
            if src.exists() && !dst.exists() && !is_link {
                if let Some(parent) = dst.parent() {
                    fs::create_dir_all(parent)?;
                }
                std::os::unix::fs::symlink(&src, &dst)?;
            }
        } else if is_link {
            fs::remove_file(&dst)?;
        }
    }
    Ok(())
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
        sync_shared(&dir.join("home"), &cfg)?;
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
    let Some(name) = profile else {
        set.push(("OUI_PROFILE".into(), SYSTEM.into()));
        return Ok(SpawnEnv { set, remove: vec![], color: "gray".into(), warning: None });
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
    Ok(SpawnEnv {
        set,
        remove: CREDENTIAL_VARS.iter().map(|s| s.to_string()).collect(),
        color: cfg.color,
        warning: None,
    })
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
}
