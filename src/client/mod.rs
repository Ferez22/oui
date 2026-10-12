//! TUI client: tab bar, the active tab's panes, status bar, tmux-style prefix keys.
//!
//! `input` turns raw stdin into daemon messages, `render` draws the screen,
//! `panel` is the right panel, `picker` the session picker and prompts.

mod clipboard;
mod copy;
mod input;
mod mouse;
mod panel;
mod picker;
mod render;

use crate::config::Config;
use crate::profile;
use crate::protocol::{ClientMsg, Layout, ServerMsg, read_msg, socket_path, write_msg};
use crate::workspace::WorkspaceSpec;
use anyhow::{Context, Result, bail};
use crate::config::Action;
use input::{Input, Mode, UiEvent};
use panel::{Here, Panel, RowAction};
use picker::{PickOutcome, Prompt, PromptOutcome, SessionPicker};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::{execute, terminal};
use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::Duration;
use tokio::net::UnixStream;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};

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
    panel: Panel,
    /// Overlay holding the keyboard while `input.mode` is `Mode::Overlay`.
    overlay: Option<Overlay>,
    /// Terminal size (cols, rows).
    term: (u16, u16),
    cwd: String,
    /// First-tab profile for sessions created from the session picker.
    new_session_profile: Option<String>,
    bg: UnboundedSender<panel::Bg>,
    // --- Copy mode / mouse.
    copy: Option<copy::Copy>,
    drag: Option<mouse::Drag>,
    /// Pane cell where the current drag started.
    drag_start: (u16, u16),
    /// Panes whose parser holds the daemon's scrollback (`ServerMsg::History`).
    have_history: HashSet<u32>,
    /// Messages produced while handling server messages, sent by the main loop.
    outbox: Vec<ClientMsg>,
}

enum Overlay {
    Panel,
    Sessions(SessionPicker),
    Rename(Prompt),
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
    let attach_cwd = cwd.clone();
    let profile_for_new = profile.clone();
    let first = move |rows, cols| ClientMsg::Attach { session, create, profile, cwd: attach_cwd, rows, cols };
    attach(cwd, config, flash, profile_for_new, first).await
}

/// Open a parsed workspace: create its session (unless running), then attach.
pub async fn open_workspace(spec: WorkspaceSpec) -> Result<()> {
    if std::env::var_os("OUI_SESSION").is_some() {
        bail!("already inside an oUI session (unset OUI_SESSION to nest anyway)");
    }
    let (config, flash) = Config::load_or_default();
    let cwd = std::env::current_dir()?.to_string_lossy().into_owned();
    let profile_for_new = config.general.default_profile.clone().filter(|p| profile::load(p).is_ok());
    attach(cwd, config, flash, profile_for_new, move |rows, cols| ClientMsg::OpenWorkspace { spec, rows, cols }).await
}

/// Connect, send `first(rows, cols)` (an attach request), then run the TUI.
async fn attach(
    cwd: String,
    config: Config,
    flash: Option<String>,
    // Profile for sessions created from the session picker.
    new_session_profile: Option<String>,
    first: impl FnOnce(u16, u16) -> ClientMsg,
) -> Result<()> {
    let stream = connect(true).await?;
    let (mut rd, mut wr) = stream.into_split();

    let (cols, rows) = terminal::size()?;
    let panel_w = panel::width(config.panel.visible, config.panel.width, cols);
    write_msg(&mut wr, &first(rows.saturating_sub(CHROME_ROWS), cols - panel_w)).await?;

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
    let mouse_on = config.general.mouse;
    if mouse_on {
        let mut out = std::io::stdout();
        out.write_all(mouse::ENABLE.as_bytes())?;
        out.flush()?;
    }
    let mut input = Input::new(cwd.clone(), config.keys.clone(), config.general.default_profile.clone());
    input.mouse = mouse_on;
    let hostname = render::hostname();
    let (bg_tx, mut bg_rx) = unbounded_channel::<panel::Bg>();
    let panel = Panel::new(config.panel.visible);
    let mut app = App {
        layout: None,
        screens: HashMap::new(),
        input,
        flash,
        config,
        hostname,
        panel,
        overlay: None,
        term: (cols, rows),
        cwd: cwd.clone(),
        new_session_profile,
        bg: bg_tx,
        copy: None,
        drag: None,
        drag_start: (0, 0),
        have_history: HashSet::new(),
        outbox: Vec::new(),
    };
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
                for msg in app.on_events() {
                    write_msg(&mut wr, &msg).await?;
                }
                if let Some(f) = app.input.flash.take() {
                    app.flash = Some(f);
                }
            }
            _ = tick.tick() => {
                // Keep an open session picker's list current.
                if matches!(app.overlay, Some(Overlay::Sessions(_))) {
                    write_msg(&mut wr, &ClientMsg::ListSessions).await?;
                }
            }
            Some(bg) = bg_rx.recv() => app.panel.on_bg(bg),
            _ = winch.recv() => {
                app.term = terminal::size()?;
                write_msg(&mut wr, &app.resize_msg()).await?;
            }
        }
        // Panel, session picker and rename: keys and actions handled here.
        for msg in app.update() {
            write_msg(&mut wr, &msg).await?;
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
        for msg in std::mem::take(&mut app.outbox) {
            write_msg(&mut wr, &msg).await?;
        }
        term.draw(|f| render::draw(&app, f))?;
    };

    terminal::disable_raw_mode()?;
    if mouse_on {
        print!("{}", mouse::DISABLE);
    }
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
                // Keep scrollback locally too, so history grows with output.
                let mut p = vt100::Parser::new(rows, cols, self.config.general.scrollback);
                p.process(&data);
                self.screens.insert(pane, p);
                self.have_history.remove(&pane);
                // Size changed under copy mode: positions are meaningless now.
                if self.copy.as_ref().is_some_and(|c| c.pane == pane) {
                    self.exit_copy();
                }
            }
            ServerMsg::History { pane, rows, cols, history, data } => {
                let mut p = vt100::Parser::new(rows, cols, self.config.general.scrollback.max(history as usize));
                p.process(&data);
                let old = self.screens.insert(pane, p);
                self.have_history.insert(pane);
                if let (Some(c), Some(mut old)) = (self.copy.as_mut().filter(|c| c.pane == pane), old) {
                    let old_off = old.screen().scrollback();
                    let old_hist = crate::scrollback::history_len(old.screen_mut());
                    if let Some(p) = self.screens.get_mut(&pane) {
                        c.on_history(p, old_hist, old_off);
                    }
                    for k in std::mem::take(&mut c.queued) {
                        self.copy_key(&k);
                    }
                }
            }
            ServerMsg::Output { pane, data } => {
                if let Some(p) = self.screens.get_mut(&pane) {
                    p.process(&data);
                    if let Some(c) = self.copy.as_mut().filter(|c| c.pane == pane) {
                        c.sync(p);
                    }
                }
            }
            ServerMsg::Sessions(list) => self.panel.sessions = list,
            ServerMsg::Clipboard(b64) => clipboard::copy_from_app(&b64),
            ServerMsg::Error(e) => self.flash = Some(e),
            _ => {}
        }
    }

    /// Resize message for the pane area (terminal minus bars and panel).
    fn resize_msg(&self) -> ClientMsg {
        let (cols, rows) = self.term;
        ClientMsg::Resize { rows: rows.saturating_sub(CHROME_ROWS), cols: cols - self.panel_width() }
    }

    fn panel_width(&self) -> u16 {
        panel::width(self.panel.visible, self.config.panel.width, self.term.0)
    }

    /// Where the client is: active tab's profile and colour, session, tab count.
    fn here(&self) -> Here<'_> {
        let layout = self.layout.as_ref();
        let tab = layout.and_then(|l| l.tabs.get(l.active));
        Here {
            profile: tab.map_or(profile::SYSTEM, |t| t.profile.as_str()),
            profile_color: tab.map_or(ratatui::style::Color::Gray, |t| render::color(&t.color)),
            session: layout.map_or("", |l| l.session.as_str()),
            tabs: layout.map_or(0, |l| l.tabs.len()),
        }
    }

    /// Attach this client to another (or a new) session.
    fn switch_msg(&self, session: String, create: bool) -> ClientMsg {
        let (cols, rows) = self.term;
        ClientMsg::Attach {
            session: Some(session),
            create,
            profile: self.new_session_profile.clone(),
            cwd: self.cwd.clone(),
            rows: rows.saturating_sub(CHROME_ROWS),
            cols: cols - self.panel_width(),
        }
    }

    fn close_overlay(&mut self) {
        self.overlay = None;
        self.panel.focused = false;
        // Input may have moved on already (the prefix after an overlay key in
        // the same read): only leave the overlay's own mode.
        if matches!(self.input.mode, Mode::Overlay) {
            self.input.mode = Mode::Normal;
        }
    }

    /// Handle client-side events from the last input, then refresh the panel
    /// if due. Returns messages for the daemon.
    fn update(&mut self) -> Vec<ClientMsg> {
        let mut out = Vec::new();
        // The panel was focused when the prefix left it: `prefix Tab` toggles.
        let mut left_panel = false;
        for ev in std::mem::take(&mut self.input.ui) {
            match ev {
                UiEvent::Leave => {
                    left_panel = matches!(self.overlay, Some(Overlay::Panel));
                    self.close_overlay();
                }
                UiEvent::Action(a) => {
                    if a == Action::FocusPanel && left_panel {
                        self.close_overlay();
                    } else {
                        self.on_action(a, &mut out);
                    }
                    left_panel = false;
                }
                UiEvent::Key(k) => match self.overlay {
                    Some(_) => self.overlay_key(&k, &mut out),
                    // The overlay closed earlier in this read: the key is the pane's.
                    None => out.push(ClientMsg::Input(k)),
                },
            }
        }
        if self.layout.is_some() {
            let profile = self.here().profile.to_string();
            if self.panel.refresh(&profile, false, &self.bg) {
                out.push(ClientMsg::ListSessions);
            }
        }
        out
    }

    fn set_panel(&mut self, visible: bool, out: &mut Vec<ClientMsg>) {
        if self.panel.visible == visible {
            return;
        }
        self.panel.visible = visible;
        out.push(self.resize_msg());
        if visible {
            let profile = self.here().profile.to_string();
            if self.panel.refresh(&profile, true, &self.bg) {
                out.push(ClientMsg::ListSessions);
            }
        }
    }

    fn on_action(&mut self, action: Action, out: &mut Vec<ClientMsg>) {
        if matches!(action, Action::FocusPanel | Action::SessionPicker | Action::RenameTab) {
            // An earlier key of the same read may have closed another overlay.
            self.input.mode = Mode::Overlay;
            self.exit_copy();
        }
        match action {
            Action::TogglePanel => {
                let visible = !self.panel.visible;
                self.set_panel(visible, out);
                if !visible && matches!(self.overlay, Some(Overlay::Panel)) {
                    self.close_overlay();
                }
            }
            Action::FocusPanel => {
                self.set_panel(true, out);
                if self.panel_width() == 0 {
                    self.flash = Some("terminal too narrow for the panel".into());
                    return self.close_overlay();
                }
                self.panel.focused = true;
                self.overlay = Some(Overlay::Panel);
            }
            Action::SessionPicker => {
                let here = self.here();
                self.overlay = Some(Overlay::Sessions(SessionPicker::new(&self.panel.sessions, here.session)));
                out.push(ClientMsg::ListSessions);
            }
            Action::RenameTab => {
                let name = self.layout.as_ref().and_then(|l| l.tabs.get(l.active)).and_then(|t| t.name.clone());
                self.overlay = Some(Overlay::Rename(Prompt::new("rename tab", name.as_deref().unwrap_or(""))));
            }
            _ => {}
        }
    }

    fn overlay_key(&mut self, key: &[u8], out: &mut Vec<ClientMsg>) {
        match self.overlay.take() {
            Some(Overlay::Panel) => {
                self.overlay = Some(Overlay::Panel);
                self.panel_key(key, out);
            }
            Some(Overlay::Rename(mut p)) => match p.key(key) {
                PromptOutcome::Pending => self.overlay = Some(Overlay::Rename(p)),
                PromptOutcome::Cancel => self.close_overlay(),
                PromptOutcome::Submit(name) => {
                    out.push(ClientMsg::RenameTab((!name.is_empty()).then_some(name)));
                    self.close_overlay();
                }
            },
            Some(Overlay::Sessions(mut picker)) => {
                let current = self.here().session.to_string();
                match picker.key(key, &self.panel.sessions) {
                    PickOutcome::Stay => self.overlay = Some(Overlay::Sessions(picker)),
                    PickOutcome::Close => self.close_overlay(),
                    PickOutcome::Switch(name) => {
                        if name != current {
                            out.push(self.switch_msg(name, false));
                        }
                        self.close_overlay();
                    }
                    PickOutcome::Create(name) => {
                        out.push(self.switch_msg(name, true));
                        out.push(ClientMsg::ListSessions);
                        self.close_overlay();
                    }
                    PickOutcome::Kill(name) => {
                        // Killing our own session: move to another one first if there is one.
                        if name == current
                            && let Some(other) = self.panel.sessions.iter().find(|s| s.name != name)
                        {
                            out.push(self.switch_msg(other.name.clone(), false));
                        }
                        out.push(ClientMsg::KillSession(name.clone()));
                        // Panes exit asynchronously; drop it locally until the next list.
                        self.panel.sessions.retain(|s| s.name != name);
                        self.overlay = Some(Overlay::Sessions(picker));
                    }
                }
            }
            None => {}
        }
    }

    /// Panel focus: j/k/arrows move, Enter acts on the row, Esc/q/Tab go back to the panes.
    fn panel_key(&mut self, key: &[u8], out: &mut Vec<ClientMsg>) {
        let rows = |app: &App| app.panel.rows(&app.config.panel.widgets, &app.here(), ratatui::style::Color::Reset);
        match key {
            b"j" | b"\x1b[B" => {
                let r = rows(self);
                self.panel.move_sel(&r, 1);
            }
            b"k" | b"\x1b[A" => {
                let r = rows(self);
                self.panel.move_sel(&r, -1);
            }
            b"\x1b" | b"q" | b"\x03" | b"\t" => self.close_overlay(),
            b"\r" | b"\n" => {
                let Some(action) = self.panel.selected(rows(self)) else { return };
                let profile = self.here().profile.to_string();
                let tab_profile = |p: String| (p != profile::SYSTEM).then_some(p);
                match action {
                    RowAction::Login { tool, cmd, hint } => {
                        if let Some(cmd) = cmd {
                            out.push(ClientMsg::NewTab { profile: tab_profile(profile.clone()), cwd: self.cwd.clone(), cmd: Some(cmd) });
                        }
                        self.flash = Some(hint.unwrap_or_else(|| format!("{tool}: logging in for profile '{profile}'")));
                    }
                    RowAction::Session(name) => {
                        if name != self.here().session {
                            out.push(self.switch_msg(name, false));
                        }
                    }
                    RowAction::Profile(name) => {
                        out.push(ClientMsg::NewTab { profile: tab_profile(name), cwd: self.cwd.clone(), cmd: None });
                    }
                }
                self.close_overlay();
            }
            _ => {}
        }
    }
    /// Drain the input events `Input::feed` queued (copy keys, mouse).
    fn on_events(&mut self) -> Vec<ClientMsg> {
        let mut out = Vec::new();
        for ev in std::mem::take(&mut self.input.events) {
            match ev {
                input::Event::EnterCopy => {
                    let focused = self.layout.as_ref().and_then(|l| l.tabs.get(l.active)).map(|t| t.focused);
                    match focused {
                        Some(pane) => self.enter_copy(pane, false),
                        None => self.input.mode = Mode::Normal,
                    }
                }
                input::Event::ExitCopy => self.exit_copy(), // input is in prefix mode already
                input::Event::CopyKey(k) => self.copy_key(&k),
                input::Event::Mouse(ev) => out.extend(self.on_mouse(ev)),
            }
        }
        out.append(&mut self.outbox);
        out
    }
}

