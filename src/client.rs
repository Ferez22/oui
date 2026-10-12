//! TUI client: tab bar, active pane, status bar, tmux-style prefix keys.
//!
//! Input is read as raw bytes and forwarded to the pane untouched, except the
//! prefix key (Ctrl-b) which starts an oUI command.

use crate::profile;
use crate::protocol::{ClientMsg, Layout, ServerMsg, read_msg, socket_path, write_msg};
use anyhow::{Context, Result, bail};
use ratatui::backend::CrosstermBackend;
use ratatui::buffer::Buffer;
use ratatui::crossterm::{execute, terminal};
use ratatui::layout::{Constraint, Layout as Split, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};
use ratatui::{Frame, Terminal};
use std::collections::HashMap;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::Duration;
use tokio::net::UnixStream;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc::unbounded_channel;

const PREFIX: u8 = 0x02; // Ctrl-b
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

enum Mode {
    Normal,
    Prefix,
    /// New-tab profile picker; index into `picker_options`.
    Picker(usize),
}

struct App {
    layout: Option<Layout>,
    screens: HashMap<u32, vt100::Parser>,
    mode: Mode,
    flash: Option<String>,
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
    let mut app = App { layout: None, screens: HashMap::new(), mode: Mode::Normal, flash: None };

    let reason = loop {
        tokio::select! {
            msg = srv_rx.recv() => match msg {
                None => break "lost connection to oUI server".to_string(),
                Some(ServerMsg::Exit(reason)) => break reason,
                Some(msg) => app.on_server(msg),
            },
            Some(bytes) = in_rx.recv() => {
                for msg in app.on_input(&bytes, &cwd) {
                    write_msg(&mut wr, &msg).await?;
                }
            }
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
        term.draw(|f| app.draw(f))?;
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
                self.screens.retain(|id, _| l.tabs.iter().any(|t| t.pane == *id));
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

    fn on_input(&mut self, bytes: &[u8], cwd: &str) -> Vec<ClientMsg> {
        let mut out = Vec::new();
        let mut pass = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            let b = bytes[i];
            i += 1;
            match self.mode {
                Mode::Normal if b == PREFIX => {
                    self.mode = Mode::Prefix;
                    self.flash = None;
                }
                Mode::Normal => pass.push(b),
                Mode::Prefix => {
                    if !pass.is_empty() {
                        out.push(ClientMsg::Input(std::mem::take(&mut pass)));
                    }
                    self.mode = Mode::Normal;
                    match b {
                        b'c' => self.mode = Mode::Picker(0),
                        b'n' => out.push(ClientMsg::NextTab),
                        b'p' => out.push(ClientMsg::PrevTab),
                        b'1'..=b'9' => out.push(ClientMsg::SelectTab((b - b'1') as usize)),
                        b'x' => out.push(ClientMsg::CloseTab),
                        b'd' => out.push(ClientMsg::Detach),
                        PREFIX => out.push(ClientMsg::Input(vec![PREFIX])),
                        _ => {}
                    }
                }
                Mode::Picker(sel) => {
                    let opts = picker_options();
                    let n = opts.len();
                    let pick = |idx: usize| ClientMsg::NewTab {
                        profile: (idx > 0).then(|| opts[idx].0.clone()),
                        cwd: cwd.to_string(),
                    };
                    match b {
                        0x1b if bytes.get(i) == Some(&b'[') && i + 1 < bytes.len() => {
                            match bytes[i + 1] {
                                b'A' => self.mode = Mode::Picker((sel + n - 1) % n),
                                b'B' => self.mode = Mode::Picker((sel + 1) % n),
                                _ => {}
                            }
                            i += 2;
                        }
                        0x1b | b'q' | 0x03 => self.mode = Mode::Normal,
                        b'k' => self.mode = Mode::Picker((sel + n - 1) % n),
                        b'j' => self.mode = Mode::Picker((sel + 1) % n),
                        b'\r' | b'\n' => {
                            out.push(pick(sel));
                            self.mode = Mode::Normal;
                        }
                        b'0'..=b'9' if ((b - b'0') as usize) < n => {
                            out.push(pick((b - b'0') as usize));
                            self.mode = Mode::Normal;
                        }
                        _ => {}
                    }
                }
            }
        }
        if !pass.is_empty() {
            out.push(ClientMsg::Input(pass));
        }
        out
    }

    fn draw(&self, f: &mut Frame) {
        let [tabs_area, pane_area, status_area] =
            Split::vertical([Constraint::Length(1), Constraint::Min(1), Constraint::Length(1)]).areas(f.area());
        let Some(layout) = &self.layout else {
            f.render_widget(Paragraph::new(" connecting…"), pane_area);
            return;
        };
        let active = layout.tabs.get(layout.active);

        // Tab bar: each tab shows its number and profile, in the profile's colour.
        let mut spans = vec![Span::styled(" oUI ", Style::new().add_modifier(Modifier::BOLD))];
        for (i, t) in layout.tabs.iter().enumerate() {
            let c = color(&t.color);
            let style = if i == layout.active {
                Style::new().bg(c).fg(Color::Black).add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(c)
            };
            spans.push(Span::styled(format!(" {} {} ", i + 1, t.profile), style));
            spans.push(Span::raw(" "));
        }
        f.render_widget(Paragraph::new(Line::from(spans)), tabs_area);

        // Active pane.
        if let Some(screen) = active.and_then(|t| self.screens.get(&t.pane)).map(|p| p.screen()) {
            render_screen(screen, pane_area, f.buffer_mut());
            if !screen.hide_cursor() && matches!(self.mode, Mode::Normal) {
                let (r, c) = screen.cursor_position();
                if r < pane_area.height && c < pane_area.width {
                    f.set_cursor_position((pane_area.x + c, pane_area.y + r));
                }
            }
        }

        // Status bar.
        let (pname, pcolor) = active.map(|t| (t.profile.as_str(), color(&t.color))).unwrap_or(("-", Color::Gray));
        let mode = match self.mode {
            Mode::Normal => Span::raw(""),
            _ => Span::styled(" PREFIX ", Style::new().bg(Color::Yellow).fg(Color::Black)),
        };
        let left = Line::from(vec![
            mode,
            Span::styled(format!(" {} ", layout.session), Style::new().add_modifier(Modifier::BOLD)),
            Span::raw("│"),
            Span::styled(format!(" ● {pname} "), Style::new().fg(pcolor)),
        ]);
        let hint = self.flash.clone().unwrap_or_else(|| "^b c new · n/p/1-9 switch · x close · d detach ".into());
        let [l, r] = Split::horizontal([Constraint::Min(1), Constraint::Length(hint.chars().count() as u16)])
            .areas(status_area);
        let bar = Style::new().bg(Color::Indexed(236));
        f.render_widget(Paragraph::new(left).style(bar), l);
        f.render_widget(Paragraph::new(hint).style(bar.fg(Color::Gray)), r);

        if let Mode::Picker(sel) = self.mode {
            self.draw_picker(f, sel);
        }
    }

    fn draw_picker(&self, f: &mut Frame, sel: usize) {
        let opts = picker_options();
        let h = opts.len() as u16 + 2;
        let area = f.area();
        let rect = Rect {
            x: area.width.saturating_sub(36) / 2,
            y: area.height.saturating_sub(h) / 2,
            width: 36.min(area.width),
            height: h.min(area.height),
        };
        let items: Vec<ListItem> = opts
            .iter()
            .enumerate()
            .map(|(i, (name, c))| ListItem::new(Line::from(vec![
                Span::raw(format!(" {i} ")),
                Span::styled("● ", Style::new().fg(color(c))),
                Span::raw(name.clone()),
            ])))
            .collect();
        let list = List::new(items)
            .block(Block::new().borders(Borders::ALL).title(" new tab · profile "))
            .highlight_style(Style::new().add_modifier(Modifier::REVERSED));
        let mut state = ListState::default().with_selected(Some(sel));
        f.render_widget(Clear, rect);
        f.render_stateful_widget(list, rect, &mut state);
    }
}

fn color(name: &str) -> Color {
    name.parse().unwrap_or(Color::Blue)
}

fn vt_color(c: vt100::Color) -> Color {
    match c {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(i) => Color::Indexed(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

fn render_screen(screen: &vt100::Screen, area: Rect, buf: &mut Buffer) {
    let (rows, cols) = screen.size();
    for row in 0..rows.min(area.height) {
        for col in 0..cols.min(area.width) {
            let Some(cell) = screen.cell(row, col) else { continue };
            if cell.is_wide_continuation() {
                continue;
            }
            let mut style = Style::new().fg(vt_color(cell.fgcolor())).bg(vt_color(cell.bgcolor()));
            for (on, m) in [
                (cell.bold(), Modifier::BOLD),
                (cell.dim(), Modifier::DIM),
                (cell.italic(), Modifier::ITALIC),
                (cell.underline(), Modifier::UNDERLINED),
                (cell.inverse(), Modifier::REVERSED),
            ] {
                if on {
                    style = style.add_modifier(m);
                }
            }
            let sym = if cell.has_contents() { cell.contents() } else { " " };
            buf[(area.x + col, area.y + row)].set_symbol(sym).set_style(style);
        }
    }
}
