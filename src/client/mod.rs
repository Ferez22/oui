//! TUI client: tab bar, the active tab's panes, status bar, tmux-style prefix keys.
//!
//! `input` turns raw stdin into daemon messages, `render` draws the screen.

mod input;
mod render;

use crate::config::Config;
use crate::profile;
use crate::protocol::{ClientMsg, Layout, ServerMsg, read_msg, socket_path, write_msg};
use anyhow::{Context, Result, bail};
use input::{Input, Mode};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::{execute, terminal};
use std::collections::HashMap;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::Duration;
use tokio::net::UnixStream;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc::unbounded_channel;

const CHROME_ROWS: u16 = 2; // tab bar + status bar

/// Connect to the daemon, starting it if needed.
pub async fn connect(spawn: bool) -> Result<UnixStream> {
    let path = socket_path()?;
    if let Ok(s) = UnixStream::connect(&path).await {
        return Ok(s);
    }
    if !spawn {
        bail!("no oUI server running");
    }
    let root = profile::oui_root()?;
    std::fs::create_dir_all(&root)?;
    let log = std::fs::OpenOptions::new().create(true).append(true).open(root.join("daemon.log"))?;
    let mut cmd = Command::new(std::env::current_exe()?);
    cmd.arg("daemon").stdin(Stdio::null()).stdout(log.try_clone()?).stderr(log);
    // Started from inside a profile shell: give the daemon the real environment
    // back so "system" tabs aren't isolated by accident.
    if std::env::var_os("OUI_PROFILE").is_some() {
        cmd.env("HOME", profile::real_home()?);
        for v in ["XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME", "CLAUDE_CONFIG_DIR",
                  "GH_CONFIG_DIR", "OUI_PROFILE", "OUI_PROFILE_COLOR", "OUI_REAL_HOME"] {
            cmd.env_remove(v);
        }
    }
    // SAFETY: setsid is async-signal-safe; detaches the daemon from our terminal.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    cmd.spawn().context("start oUI daemon")?;
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        if let Ok(s) = UnixStream::connect(&path).await {
            return Ok(s);
        }
    }
    bail!("oUI daemon did not start (see {})", root.join("daemon.log").display())
}

/// Send one request and print the reply (ls, kill, kill-server).
pub async fn request(msg: ClientMsg) -> Result<()> {
    let mut s = connect(false).await?;
    write_msg(&mut s, &msg).await?;
    match read_msg::<_, ServerMsg>(&mut s).await? {
        ServerMsg::Sessions(list) if list.is_empty() => println!("no sessions"),
        ServerMsg::Sessions(list) => {
            for s in list {
                let attached = if s.clients > 0 { " (attached)" } else { "" };
                println!("{}: {} tab(s){attached}", s.name, s.tabs);
            }
        }
        ServerMsg::Error(e) => bail!(e),
        _ => {}
    }
    Ok(())
}

struct App {
    layout: Option<Layout>,
    screens: HashMap<u32, vt100::Parser>,
    input: Input,
    flash: Option<String>,
    config: Config,
    /// Status bar `hostname` segment (looked up once).
    hostname: String,
}

fn picker_options() -> Vec<(String, String)> {
    let mut v = vec![(profile::SYSTEM.to_string(), "gray".to_string())];
    v.extend(profile::list().into_iter().map(|p| (p.name, p.color)));
    v
}

pub async fn run(session: Option<String>, create: bool, profile: Option<String>) -> Result<()> {
    if std::env::var_os("OUI_SESSION").is_some() {
        bail!("already inside an oUI session (unset OUI_SESSION to nest anyway)");
    }
    let (config, mut flash) = Config::load_or_default();
    // No -p: use general.default_profile, falling back to system if it's gone.
    let profile = match profile {
        Some(p) => Some(p),
        None => match config.general.default_profile.clone() {
            Some(d) if d != profile::SYSTEM => match profile::load(&d) {
                Ok(_) => Some(d),
                Err(e) => {
                    flash = Some(format!("general.default_profile: {e:#}; using {}", profile::SYSTEM));
                    None
                }
            },
            _ => None,
        },
    };
    let profile = profile.filter(|p| p != profile::SYSTEM);
    if let Some(p) = &profile {
        profile::load(p)?;
    }
    let cwd = std::env::current_dir()?.to_string_lossy().into_owned();
    let stream = connect(true).await?;
    let (mut rd, mut wr) = stream.into_split();

    let (cols, rows) = terminal::size()?;
    write_msg(&mut wr, &ClientMsg::Attach {
        session,
        create,
        profile,
        cwd: cwd.clone(),
        rows: rows.saturating_sub(CHROME_ROWS),
        cols,
    })
    .await?;

    let (srv_tx, mut srv_rx) = unbounded_channel::<ServerMsg>();
    tokio::spawn(async move {
        while let Ok(msg) = read_msg::<_, ServerMsg>(&mut rd).await {
            if srv_tx.send(msg).is_err() {
                break;
            }
        }
    });
    let (in_tx, mut in_rx) = unbounded_channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        let mut buf = [0u8; 4096];
        while let Ok(n) = stdin.read(&mut buf) {
            if n == 0 || in_tx.send(buf[..n].to_vec()).is_err() {
                break;
            }
        }
    });
    let mut winch = signal(SignalKind::window_change())?;

    terminal::enable_raw_mode()?;
    execute!(std::io::stdout(), terminal::EnterAlternateScreen)?;
    let mut term = Terminal::new(CrosstermBackend::new(std::io::stdout()))?;
    let input = Input::new(cwd.clone(), config.keys.clone(), config.general.default_profile.clone());
    let hostname = render::hostname();
    let mut app = App { layout: None, screens: HashMap::new(), input, flash, config, hostname };
    // Redraw at least every second so the clock segment stays current.
    let mut tick = tokio::time::interval(Duration::from_secs(1));

    let reason = loop {
        tokio::select! {
            msg = srv_rx.recv() => match msg {
                None => break "lost connection to oUI server".to_string(),
                Some(ServerMsg::Exit(reason)) => break reason,
                Some(msg) => app.on_server(msg),
            },
            Some(bytes) = in_rx.recv() => {
                if matches!(app.input.mode, Mode::Normal) {
                    app.flash = None;
                }
                for msg in app.input.feed(&bytes) {
                    write_msg(&mut wr, &msg).await?;
                }
                if let Some(f) = app.input.flash.take() {
                    app.flash = Some(f);
                }
            }
            _ = tick.tick() => {}
            _ = winch.recv() => {
                let (cols, rows) = terminal::size()?;
                write_msg(&mut wr, &ClientMsg::Resize { rows: rows.saturating_sub(CHROME_ROWS), cols }).await?;
            }
        }
        // Coalesce bursts of output into one redraw.
        let mut exit = None;
        while let Ok(msg) = srv_rx.try_recv() {
            match msg {
                ServerMsg::Exit(reason) => {
                    exit = Some(reason);
                    break;
                }
                msg => app.on_server(msg),
            }
        }
        if let Some(reason) = exit {
            break reason;
        }
        term.draw(|f| render::draw(&app, f))?;
    };

    terminal::disable_raw_mode()?;
    execute!(std::io::stdout(), terminal::LeaveAlternateScreen)?;
    println!("[oUI: {reason}]");
    // The stdin thread is blocked on read; exit instead of joining it.
    std::process::exit(0);
}

impl App {
    fn on_server(&mut self, msg: ServerMsg) {
        match msg {
            ServerMsg::Layout(l) => {
                self.screens.retain(|id, _| l.tabs.iter().any(|t| t.panes.iter().any(|p| p.id == *id)));
                self.layout = Some(l);
            }
            ServerMsg::Snapshot { pane, rows, cols, data } => {
                let mut p = vt100::Parser::new(rows, cols, 0);
                p.process(&data);
                self.screens.insert(pane, p);
            }
            ServerMsg::Output { pane, data } => {
                if let Some(p) = self.screens.get_mut(&pane) {
                    p.process(&data);
                }
            }
            ServerMsg::Error(e) => self.flash = Some(e),
            _ => {}
        }
    }
}
