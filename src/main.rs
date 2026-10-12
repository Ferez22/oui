//! oUI isolation spike.
//!
//! Proves that every CLI tool can keep a separate login per profile by giving
//! each profile its own HOME and XDG directories. This module becomes the core
//! of the future daemon: every shell oUI spawns goes through `profile_env`.

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::{env, fs};

/// Non-credential files and folders linked from the real HOME into a profile
/// when `shared = true`, so shells, prompts and toolchains keep working.
/// Credential locations (e.g. ~/.railway, ~/.config/gh, ~/.claude) are never linked.
const SHARED_ALLOWLIST: &[&str] = &[
    ".zshrc", ".zprofile", ".zshenv", ".zlogin", ".bashrc", ".bash_profile", ".profile",
    ".inputrc", ".vimrc", ".vim", ".nanorc", ".tmux.conf", ".gitignore_global",
    ".oh-my-zsh", ".nvm", ".cargo", ".rustup", ".bun", ".deno", ".pyenv", ".rbenv",
    ".sdkman", ".local/bin", ".config/starship.toml", ".config/nvim",
];

#[derive(Parser)]
#[command(name = "oui", about = "oUI: terminal profiles with isolated logins")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Manage profiles
    #[command(subcommand)]
    Profile(ProfileCmd),
    /// Open an interactive shell inside a profile
    Shell { name: String },
    /// Run one command inside a profile: oui run work -- railway whoami
    Run {
        name: String,
        #[arg(last = true, required = true)]
        cmd: Vec<String>,
    },
    /// Print the environment a profile's shells get
    Env { name: String },
}

#[derive(Subcommand)]
enum ProfileCmd {
    /// Create a profile (shared dotfiles by default, --fresh for none)
    New {
        name: String,
        #[arg(long)]
        fresh: bool,
        #[arg(long, default_value = "blue")]
        color: String,
    },
    /// List profiles
    List,
    /// Toggle sharing of dotfiles from the real HOME
    Share { name: String, #[arg(action = clap::ArgAction::Set)] on: bool },
}

#[derive(Serialize, Deserialize)]
struct ProfileConfig {
    name: String,
    color: String,
    shared: bool,
}

fn oui_root() -> Result<PathBuf> {
    if let Ok(p) = env::var("OUI_HOME") {
        return Ok(PathBuf::from(p));
    }
    Ok(real_home()?.join(".oui"))
}

/// The user's real home, even when called from inside a profile.
fn real_home() -> Result<PathBuf> {
    env::var("OUI_REAL_HOME")
        .or_else(|_| env::var("HOME"))
        .map(PathBuf::from)
        .context("HOME not set")
}

fn profile_dir(name: &str) -> Result<PathBuf> {
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        bail!("profile name must be letters, digits, '-' or '_'");
    }
    Ok(oui_root()?.join("profiles").join(name))
}

fn load(name: &str) -> Result<(PathBuf, ProfileConfig)> {
    let dir = profile_dir(name)?;
    let raw = fs::read_to_string(dir.join("profile.toml"))
        .with_context(|| format!("profile '{name}' not found (create it: oui profile new {name})"))?;
    Ok((dir, toml::from_str(&raw)?))
}

fn save(dir: &Path, cfg: &ProfileConfig) -> Result<()> {
    fs::write(dir.join("profile.toml"), toml::to_string_pretty(cfg)?)?;
    Ok(())
}

/// Link (or unlink) allowlisted dotfiles. Only removes entries that are our symlinks.
fn sync_shared(home: &Path, shared: bool) -> Result<()> {
    let real = real_home()?;
    for rel in SHARED_ALLOWLIST {
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

/// Environment every process in the profile gets. Tools that honour HOME/XDG
/// isolate automatically; tool-specific vars cover the rest.
fn profile_env(name: &str, dir: &Path, cfg: &ProfileConfig) -> Result<Vec<(String, String)>> {
    let home = dir.join("home");
    let h = |rel: &str| home.join(rel).to_string_lossy().into_owned();
    Ok(vec![
        ("OUI_PROFILE".into(), name.into()),
        ("OUI_PROFILE_COLOR".into(), cfg.color.clone()),
        ("OUI_REAL_HOME".into(), real_home()?.to_string_lossy().into_owned()),
        ("HOME".into(), home.to_string_lossy().into_owned()),
        ("XDG_CONFIG_HOME".into(), h(".config")),
        ("XDG_DATA_HOME".into(), h(".local/share")),
        ("XDG_STATE_HOME".into(), h(".local/state")),
        ("XDG_CACHE_HOME".into(), h(".cache")),
        // Tool-specific overrides (adapters). Claude Code also keys its
        // macOS Keychain entry on this dir in recent versions — verified by spike.
        ("CLAUDE_CONFIG_DIR".into(), h(".claude")),
        ("GH_CONFIG_DIR".into(), h(".config/gh")),
        // Avoid the "nano doesn't work over ssh" problem: use a TERM every server knows.
        ("TERM".into(), "xterm-256color".into()),
    ])
}

fn command_in(name: &str, program: &str, args: &[String]) -> Result<Command> {
    let (dir, cfg) = load(name)?;
    sync_shared(&dir.join("home"), cfg.shared)?;
    let mut c = Command::new(program);
    c.args(args);
    // Drop credentials that would override per-profile logins.
    for var in ["ANTHROPIC_API_KEY", "CLAUDE_CODE_OAUTH_TOKEN", "RAILWAY_TOKEN", "RAILWAY_API_TOKEN",
                "GH_TOKEN", "GITHUB_TOKEN", "VERCEL_TOKEN"] {
        c.env_remove(var);
    }
    c.envs(profile_env(name, &dir, &cfg)?);
    c.current_dir(env::current_dir().unwrap_or(dir.join("home")));
    Ok(c)
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Profile(ProfileCmd::New { name, fresh, color }) => {
            let dir = profile_dir(&name)?;
            if dir.exists() {
                bail!("profile '{name}' already exists");
            }
            let home = dir.join("home");
            for sub in [".config", ".local/share", ".local/state", ".cache", ".claude"] {
                fs::create_dir_all(home.join(sub))?;
            }
            let cfg = ProfileConfig { name: name.clone(), color, shared: !fresh };
            save(&dir, &cfg)?;
            sync_shared(&home, cfg.shared)?;
            println!("created profile '{name}' at {} (shared dotfiles: {})", dir.display(), cfg.shared);
        }
        Cmd::Profile(ProfileCmd::List) => {
            let root = oui_root()?.join("profiles");
            for entry in fs::read_dir(&root).into_iter().flatten().flatten() {
                if let Ok((_, cfg)) = load(&entry.file_name().to_string_lossy()) {
                    println!("{:<16} color={:<8} shared={}", cfg.name, cfg.color, cfg.shared);
                }
            }
        }
        Cmd::Profile(ProfileCmd::Share { name, on }) => {
            let (dir, mut cfg) = load(&name)?;
            if !on {
                eprintln!("note: unsharing removes linked dotfiles; profile logins are kept");
            }
            cfg.shared = on;
            save(&dir, &cfg)?;
            sync_shared(&dir.join("home"), on)?;
        }
        Cmd::Shell { name } => {
            let shell = env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
            eprintln!("oUI ▸ entering profile '{name}' (exit to leave)");
            return Err(command_in(&name, &shell, &["-l".into()])?.exec().into());
        }
        Cmd::Run { name, cmd } => {
            return Err(command_in(&name, &cmd[0], &cmd[1..])?.exec().into());
        }
        Cmd::Env { name } => {
            let (dir, cfg) = load(&name)?;
            for (k, v) in profile_env(&name, &dir, &cfg)? {
                println!("{k}={v}");
            }
        }
    }
    Ok(())
}
