//! oUI: terminal workspace with per-profile isolated logins and tmux-style sessions.

mod client;
mod config;
mod daemon;
mod profile;
mod protocol;

use anyhow::Result;
use clap::{Parser, Subcommand};
use protocol::ClientMsg;
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
    /// Config file (~/.oui/config.toml)
    #[command(subcommand)]
    Config(ConfigCmd),
    /// Run the server (started automatically)
    #[command(hide = true)]
    Daemon,
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
    },
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

fn exec_in(name: &str, program: &str, args: &[String]) -> Result<()> {
    let env = profile::spawn_env(Some(name))?;
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
        Some(Cmd::Profile(ProfileCmd::New { name, fresh, color })) => {
            let dir = profile::create(&name, &color, fresh)?;
            println!("created profile '{name}' at {} (shared dotfiles: {})", dir.display(), !fresh);
            Ok(())
        }
        Some(Cmd::Profile(ProfileCmd::List)) => {
            for p in profile::list() {
                println!("{:<16} color={:<8} shared={}", p.name, p.color, p.shared);
            }
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
        Some(Cmd::Env { name }) => {
            for (k, v) in profile::spawn_env(Some(&name))?.set {
                println!("{k}={v}");
            }
            Ok(())
        }
    }
}
