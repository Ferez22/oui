//! oUI: terminal workspace with per-profile isolated logins and tmux-style sessions.

mod accounts;
mod client;
mod config;
mod daemon;
mod keychain;
mod layout;
mod profile;
mod protocol;
mod workspace;

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
use protocol::ClientMsg;
use std::io::{BufRead, IsTerminal, Write};
use std::os::unix::process::CommandExt;
use std::process::Command;

#[derive(Parser)]
#[command(name = "oui", version, about = "oUI: terminal profiles with isolated logins + tmux-style sessions")]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create (or attach to) a session
    New {
        /// Session name
        #[arg(short, long)]
        session: Option<String>,
        /// Profile for the first tab (default: system)
        #[arg(short, long)]
        profile: Option<String>,
    },
    /// Attach to a session (default: most recent)
    #[command(alias = "a")]
    Attach { session: Option<String> },
    /// List sessions
    Ls,
    /// Kill a session and all its shells
    Kill { session: String },
    /// Stop the oUI server and every session
    KillServer,
    /// Manage profiles
    #[command(subcommand)]
    Profile(ProfileCmd),
    /// Open a plain shell inside a profile (no TUI)
    Shell { name: String },
    /// Run one command inside a profile: oui run work -- railway whoami
    Run {
        name: String,
        #[arg(last = true, required = true)]
        cmd: Vec<String>,
    },
    /// Print the environment a profile's shells get
    Env { name: String },
    /// Show which CLI tools are logged in for a profile (default: system)
    Accounts { profile: Option<String> },
    /// Open a workspace by name (~/.oui/workspaces/<name>.toml) or path;
    /// attaches if its session is already running
    Open { workspace: String },
    /// Manage workspace files
    #[command(subcommand, alias = "ws")]
    Workspace(WorkspaceCmd),
    /// Config file (~/.oui/config.toml)
    #[command(subcommand)]
    Config(ConfigCmd),
    /// Run the server (started automatically)
    #[command(hide = true)]
    Daemon,
}

#[derive(Subcommand)]
enum WorkspaceCmd {
    /// List workspaces in ~/.oui/workspaces
    #[command(alias = "ls")]
    List,
    /// Create a commented workspace file to edit
    New { name: String },
    /// Save a running session's layout (profiles, splits, directories) as a workspace
    Save {
        session: String,
        /// Workspace name (default: the session name)
        name: Option<String>,
        /// Overwrite an existing workspace file
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand)]
enum ConfigCmd {
    /// Print the config file path
    Path,
    /// Print the default config, commented
    Default,
    /// Check the config file for errors
    Check,
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
        /// macOS: where the profile keychain's password is kept: `secure` (your
        /// login keychain; macOS may ask once to allow oUI) or `file` (a 0600
        /// file in the profile, no prompts). Asked interactively if omitted.
        #[arg(long, value_enum)]
        keychain: Option<keychain::Mode>,
    },
    /// Switch where a profile keychain's password is kept (its logins are kept)
    Keychain {
        name: String,
        #[arg(value_enum)]
        mode: keychain::Mode,
    },
    /// Delete a profile and all its logins (asks for confirmation)
    #[command(alias = "remove")]
    Rm { name: String },
    /// List profiles
    #[command(alias = "ls")]
    List,
    /// Toggle sharing of dotfiles from the real HOME
    Share {
        name: String,
        #[arg(action = clap::ArgAction::Set)]
        on: bool,
    },
}

/// How long `oui shell` / `oui run` wait for the profile keychain (a macOS
/// access prompt may be showing; the user is right there to answer it).
const EXEC_KEYCHAIN_WAIT: std::time::Duration = std::time::Duration::from_secs(60);

fn exec_in(name: &str, program: &str, args: &[String]) -> Result<()> {
    let env = profile::spawn_env_with(Some(name), Some(EXEC_KEYCHAIN_WAIT))?;
    if let Some(w) = &env.warning {
        eprintln!("oUI ▸ warning: {w}");
    }
    let mut c = Command::new(program);
    c.args(args);
    for var in &env.remove {
        c.env_remove(var);
    }
    c.envs(env.set);
    Err(c.exec().into())
}

fn main() -> Result<()> {
    let cmd = Cli::parse().cmd;
    let rt = || tokio::runtime::Runtime::new();
    match cmd {
        None => rt()?.block_on(client::run(None, true, None)),
        Some(Cmd::New { session, profile }) => rt()?.block_on(client::run(session, true, profile)),
        Some(Cmd::Attach { session }) => rt()?.block_on(client::run(session, false, None)),
        Some(Cmd::Ls) => rt()?.block_on(client::request(ClientMsg::ListSessions)),
        Some(Cmd::Kill { session }) => rt()?.block_on(client::request(ClientMsg::KillSession(session))),
        Some(Cmd::KillServer) => rt()?.block_on(client::request(ClientMsg::KillServer)),
        Some(Cmd::Daemon) => daemon::run(),
        Some(Cmd::Open { workspace }) => rt()?.block_on(workspace::open(&workspace)),
        Some(Cmd::Workspace(WorkspaceCmd::List)) => workspace::list(),
        Some(Cmd::Workspace(WorkspaceCmd::New { name })) => workspace::new(&name),
        Some(Cmd::Workspace(WorkspaceCmd::Save { session, name, force })) => {
            rt()?.block_on(workspace::save(&session, name.as_deref(), force))
        }
        Some(Cmd::Config(ConfigCmd::Path)) => {
            println!("{}", config::path()?.display());
            Ok(())
        }
        Some(Cmd::Config(ConfigCmd::Default)) => {
            print!("{}", config::default_toml());
            Ok(())
        }
        Some(Cmd::Config(ConfigCmd::Check)) => {
            config::Config::load()?;
            println!("{}: ok", config::path()?.display());
            Ok(())
        }
        Some(Cmd::Profile(ProfileCmd::New { name, fresh, color, keychain })) => {
            profile::profile_dir(&name)?; // validate the name before asking anything
            let mode = match keychain {
                Some(m) => m,
                None => ask_keychain_mode(&name)?,
            };
            let (dir, warning) = profile::create(&name, &color, fresh, mode)?;
            println!("created profile '{name}' at {} (shared dotfiles: {})", dir.display(), !fresh);
            if cfg!(target_os = "macos") {
                println!("keychain: {}", mode.label());
            }
            if let Some(w) = warning {
                eprintln!("warning: {w}");
            }
            Ok(())
        }
        Some(Cmd::Profile(ProfileCmd::List)) => {
            for p in profile::list() {
                println!("{:<16} color={:<8} shared={:<5} keychain={}", p.name, p.color, p.shared, p.keychain.label());
            }
            Ok(())
        }
        Some(Cmd::Profile(ProfileCmd::Keychain { name, mode })) => {
            profile::set_keychain_mode(&name, mode)?;
            let place = match mode {
                keychain::Mode::Secure => "your login keychain (secure)".to_string(),
                keychain::Mode::File => format!("{} in the profile folder (file)", keychain::PASSWORD_FILE),
            };
            println!("profile '{name}': keychain password now kept in {place}");
            Ok(())
        }
        Some(Cmd::Profile(ProfileCmd::Rm { name })) => {
            let (dir, _) = profile::load(&name)?;
            eprint!(
                "Delete profile '{name}' ({}) with all its logins and keychain?\nType the profile name to confirm: ",
                dir.display()
            );
            std::io::stderr().flush()?;
            let mut answer = String::new();
            std::io::stdin().lock().read_line(&mut answer)?;
            if answer.trim() != name {
                bail!("not confirmed; nothing deleted");
            }
            if let Some(w) = profile::remove(&name)? {
                eprintln!("warning: {w}");
            }
            println!("deleted profile '{name}'");
            Ok(())
        }
        Some(Cmd::Profile(ProfileCmd::Share { name, on })) => {
            if !on {
                eprintln!("note: unsharing removes linked dotfiles; profile logins are kept");
            }
            profile::set_shared(&name, on)
        }
        Some(Cmd::Shell { name }) => {
            let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
            eprintln!("oUI ▸ entering profile '{name}' (exit to leave)");
            exec_in(&name, &shell, &["-l".into()])
        }
        Some(Cmd::Run { name, cmd }) => exec_in(&name, &cmd[0], &cmd[1..]),
        Some(Cmd::Accounts { profile }) => accounts::print_table(profile.as_deref()),
        Some(Cmd::Env { name }) => {
            for (k, v) in profile::spawn_env_with(Some(&name), None)?.set {
                println!("{k}={v}");
            }
            Ok(())
        }
    }
}

/// `oui profile new` without `--keychain`: ask on a terminal (macOS), else `secure`.
fn ask_keychain_mode(name: &str) -> Result<keychain::Mode> {
    if !cfg!(target_os = "macos") || !std::io::stdin().is_terminal() {
        return Ok(keychain::Mode::default());
    }
    eprintln!("Profile '{name}' gets its own macOS keychain. Where should oUI keep its password?");
    eprintln!("  1) secure (default): in your login keychain. macOS may ask once to let oui");
    eprintln!("     use it; choose \"Always Allow\".");
    eprintln!("  2) file: in a private file in the profile folder. No prompts; protected like");
    eprintln!("     the token files other CLIs keep in your home.");
    loop {
        eprint!("Choose [1]: ");
        std::io::stderr().flush()?;
        let mut answer = String::new();
        if std::io::stdin().lock().read_line(&mut answer)? == 0 {
            return Ok(keychain::Mode::default());
        }
        match answer.trim().to_ascii_lowercase().as_str() {
            "" | "1" | "secure" => return Ok(keychain::Mode::Secure),
            "2" | "file" => return Ok(keychain::Mode::File),
            _ => eprintln!("please answer 1 or 2"),
        }
    }
}
