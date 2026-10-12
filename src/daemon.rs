//! The oUI daemon owns every shell. Clients attach over a unix socket; shells
//! keep running when clients detach (tmux model).
//!
//! Each pane has a PTY plus a vt100 emulator holding its screen, so a client
//! attaching later gets the current screen as a snapshot, then live output.
//! Each tab holds a layout tree of panes (all in the tab's profile); the
//! daemon computes pane geometry and sizes every PTY to its rect.

use crate::layout::{Axis, Node, Rect, neighbor};
use crate::profile;
use crate::protocol::{ClientMsg, Layout, PaneInfo, ServerMsg, SessionInfo, TabInfo, read_msg, socket_path, write_msg};
use anyhow::{Context, Result, anyhow, bail};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};

enum Event {
    Output(u32, Vec<u8>),
    Exited(u32),
}

/// Fraction of a split moved per resize key.
const RESIZE_STEP: f32 = 0.05;

struct Pane {
    session: String,
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
    parser: vt100::Parser,
}

/// A tab is a profile boundary: every pane in it runs in `profile`.
struct Tab {
    /// `None` = system (no isolation).
    profile: Option<String>,
    color: String,
    root: Node,
    focused: u32,
    zoomed: bool,
}

impl Tab {
    /// Pane rects; when zoomed the focused pane covers the whole area.
    fn geometry(&self, area: Rect) -> Vec<(u32, Rect)> {
        let mut g = self.root.geometry(area);
        if self.zoomed {
            for (id, r) in &mut g {
                if *id == self.focused {
                    *r = area;
                }
            }
        }
        g
    }
}

struct Session {
    name: String,
    tabs: Vec<Tab>,
    active: usize,
    /// Size of the pane area (excludes the client's tab and status bars).
    rows: u16,
    cols: u16,
    last_used: u64,
}

impl Session {
    fn area(&self) -> Rect {
        Rect::new(0, 0, self.cols, self.rows)
    }
}

struct Client {
    session: String,
    tx: UnboundedSender<ServerMsg>,
}

struct State {
    panes: HashMap<u32, Pane>,
    sessions: Vec<Session>,
    clients: HashMap<u64, Client>,
    next_pane: u32,
    clock: u64,
    events: UnboundedSender<Event>,
    /// `general.shell` (else `$SHELL`) and `general.scrollback`, read at startup.
    shell: Option<String>,
    scrollback: usize,
}

type Shared = Arc<Mutex<State>>;

pub fn run() -> Result<()> {
    tokio::runtime::Runtime::new()?.block_on(serve())
}

async fn serve() -> Result<()> {
    let path = socket_path()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if path.exists() {
        if UnixStream::connect(&path).await.is_ok() {
            bail!("daemon already running at {}", path.display());
        }
        std::fs::remove_file(&path)?;
    }
    let listener = UnixListener::bind(&path).with_context(|| format!("bind {}", path.display()))?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;

    let (config, warning) = crate::config::Config::load_or_default();
    if let Some(w) = warning {
        eprintln!("{w}");
    }
    let (events_tx, mut events_rx) = unbounded_channel();
    let state: Shared = Arc::new(Mutex::new(State {
        panes: HashMap::new(),
        sessions: Vec::new(),
        clients: HashMap::new(),
        next_pane: 1,
        clock: 0,
        events: events_tx,
        shell: config.general.shell,
        scrollback: config.general.scrollback,
    }));

    let pump_state = state.clone();
    tokio::spawn(async move {
        while let Some(ev) = events_rx.recv().await {
            let mut st = pump_state.lock().unwrap();
            match ev {
                Event::Output(id, data) => {
                    let Some(pane) = st.panes.get_mut(&id) else { continue };
                    pane.parser.process(&data);
                    let session = pane.session.clone();
                    st.send_to_session(&session, || ServerMsg::Output { pane: id, data: data.clone() });
                }
                Event::Exited(id) => {
                    if st.remove_pane(id) {
                        shutdown(&mut st);
                    }
                }
            }
        }
    });

    let mut next_client = 0u64;
    loop {
        let (stream, _) = listener.accept().await?;
        next_client += 1;
        let st = state.clone();
        let id = next_client;
        tokio::spawn(async move {
            if let Err(e) = handle_client(st.clone(), id, stream).await {
                eprintln!("client {id}: {e:#}");
            }
            st.lock().unwrap().clients.remove(&id);
        });
    }
}

/// Exit the daemon: tell clients, remove the socket.
fn shutdown(st: &mut State) {
    for c in st.clients.values() {
        let _ = c.tx.send(ServerMsg::Exit("oUI server exited".into()));
    }
    for pane in st.panes.values_mut() {
        let _ = pane.child.kill();
    }
    if let Ok(p) = socket_path() {
        let _ = std::fs::remove_file(p);
    }
    tokio::spawn(async {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        std::process::exit(0);
    });
}

async fn handle_client(state: Shared, id: u64, stream: UnixStream) -> Result<()> {
    let (mut rd, mut wr) = stream.into_split();
    let (tx, mut rx) = unbounded_channel::<ServerMsg>();
    let writer = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            let exit = matches!(msg, ServerMsg::Exit(_));
            if write_msg(&mut wr, &msg).await.is_err() || exit {
                break;
            }
        }
    });

    loop {
        let msg: ClientMsg = match read_msg(&mut rd).await {
            Ok(m) => m,
            Err(_) => break, // client gone
        };
        let mut st = state.lock().unwrap();
        if let Err(e) = st.handle(id, msg, &tx) {
            let _ = tx.send(ServerMsg::Error(format!("{e:#}")));
        }
    }
    drop(tx);
    let _ = writer.await;
    Ok(())
}

impl State {
    fn handle(&mut self, id: u64, msg: ClientMsg, tx: &UnboundedSender<ServerMsg>) -> Result<()> {
        match msg {
            ClientMsg::Attach { session, create, profile, cwd, rows, cols } => {
                let name = match session {
                    Some(n) => n,
                    None => match self.sessions.iter().max_by_key(|s| s.last_used) {
                        Some(s) => s.name.clone(),
                        None if create => "main".into(),
                        None => bail!("no sessions"),
                    },
                };
                if !self.sessions.iter().any(|s| s.name == name) {
                    if !create {
                        bail!("no session named '{name}'");
                    }
                    self.sessions.push(Session { name: name.clone(), tabs: vec![], active: 0, rows, cols, last_used: 0 });
                    if let Err(e) = self.spawn_tab(&name, profile.as_deref(), &cwd) {
                        self.sessions.retain(|s| s.name != name);
                        return Err(e);
                    }
                }
                self.clock += 1;
                let clock = self.clock;
                self.session_mut(&name)?.last_used = clock;
                self.clients.insert(id, Client { session: name.clone(), tx: tx.clone() });
                self.resize(&name, rows, cols)?;
            }
            ClientMsg::Input(data) => {
                let pane = self.focused_pane(id)?;
                let p = self.panes.get_mut(&pane).ok_or_else(|| anyhow!("pane gone"))?;
                p.writer.write_all(&data)?;
                p.writer.flush()?;
            }
            ClientMsg::Resize { rows, cols } => {
                let name = self.client_session(id)?;
                self.resize(&name, rows, cols)?;
            }
            ClientMsg::NewTab { profile, cwd } => {
                let name = self.client_session(id)?;
                let pane = self.spawn_tab(&name, profile.as_deref(), &cwd)?;
                self.send_snapshot(pane);
                self.send_layout(&name);
            }
            ClientMsg::SelectTab(i) => self.select(id, |_, n| i.min(n - 1))?,
            ClientMsg::NextTab => self.select(id, |a, n| (a + 1) % n)?,
            ClientMsg::PrevTab => self.select(id, |a, n| (a + n - 1) % n)?,
            ClientMsg::CloseTab => {
                let (_, leaves) = self.with_tab(id, |t, _| t.root.leaves())?;
                for pane in leaves {
                    self.kill_pane(pane);
                }
            }
            ClientMsg::ClosePane => {
                let pane = self.focused_pane(id)?;
                self.kill_pane(pane); // reader thread sees EOF → Exited → pane removed
            }
            ClientMsg::Split { down, cwd } => self.split(id, if down { Axis::Vertical } else { Axis::Horizontal }, cwd)?,
            ClientMsg::Focus(dir) => {
                let (name, _) = self.with_tab(id, |t, area| {
                    if let Some(n) = neighbor(&t.root.geometry(area), t.focused, dir) {
                        t.focused = n;
                        t.zoomed = false;
                    }
                })?;
                self.relayout(&name, false)?;
            }
            ClientMsg::ResizePane(dir) => {
                let (name, _) = self.with_tab(id, |t, _| {
                    t.zoomed = false;
                    t.root.resize(t.focused, dir, RESIZE_STEP)
                })?;
                self.relayout(&name, false)?;
            }
            ClientMsg::Zoom => {
                let (name, _) = self.with_tab(id, |t, _| t.zoomed = !t.zoomed && matches!(t.root, Node::Split { .. }))?;
                self.relayout(&name, false)?;
            }
            ClientMsg::Detach => {
                if let Some(c) = self.clients.remove(&id) {
                    let _ = c.tx.send(ServerMsg::Exit(format!("detached from session '{}'", c.session)));
                }
            }
            ClientMsg::ListSessions => {
                let mut list: Vec<_> = self.sessions.iter().collect();
                list.sort_by_key(|s| std::cmp::Reverse(s.last_used));
                let info = list
                    .iter()
                    .map(|s| SessionInfo {
                        name: s.name.clone(),
                        tabs: s.tabs.len(),
                        clients: self.clients.values().filter(|c| c.session == s.name).count(),
                    })
                    .collect();
                tx.send(ServerMsg::Sessions(info))?;
            }
            ClientMsg::KillSession(name) => {
                let panes: Vec<u32> = self.session_mut(&name)?.tabs.iter().flat_map(|t| t.root.leaves()).collect();
                for pane in panes {
                    self.kill_pane(pane);
                }
                tx.send(ServerMsg::Ok)?;
            }
            ClientMsg::KillServer => {
                tx.send(ServerMsg::Ok)?;
                shutdown(self);
            }
        }
        Ok(())
    }

    /// Spawn a shell in `profile` with a `rows`×`cols` PTY. Returns its pane id and the profile colour.
    fn spawn_pane(&mut self, session: &str, profile: Option<&str>, cwd: &str, rows: u16, cols: u16) -> Result<(u32, String)> {
        let env = profile::spawn_env(profile)?;
        let (rows, cols) = (rows.max(1), cols.max(1));
        let pair = native_pty_system().openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })?;
        let shell = self.shell.clone().or_else(|| std::env::var("SHELL").ok()).unwrap_or_else(|| "/bin/sh".into());
        let mut cmd = CommandBuilder::new(&shell);
        cmd.arg("-l");
        if Path::new(cwd).is_dir() {
            cmd.cwd(cwd);
        }
        for var in &env.remove {
            cmd.env_remove(var);
        }
        for (k, v) in &env.set {
            cmd.env(k, v);
        }
        cmd.env("OUI_SESSION", session);
        let child = pair.slave.spawn_command(cmd)?;
        drop(pair.slave); // so the reader sees EOF when the shell exits

        let id = self.next_pane;
        self.next_pane += 1;
        let mut reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;
        let events = self.events.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 65536];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if events.send(Event::Output(id, buf[..n].to_vec())).is_err() {
                            return;
                        }
                    }
                }
            }
            let _ = events.send(Event::Exited(id));
        });

        self.panes.insert(id, Pane {
            session: session.into(),
            master: pair.master,
            writer,
            child,
            parser: vt100::Parser::new(rows, cols, self.scrollback),
        });
        Ok((id, env.color))
    }

    fn spawn_tab(&mut self, session: &str, profile: Option<&str>, cwd: &str) -> Result<u32> {
        let (rows, cols) = {
            let s = self.session_mut(session)?;
            (s.rows, s.cols)
        };
        let (id, color) = self.spawn_pane(session, profile, cwd, rows, cols)?;
        let s = self.session_mut(session)?;
        s.tabs.push(Tab { profile: profile.map(Into::into), color, root: Node::Leaf(id), focused: id, zoomed: false });
        s.active = s.tabs.len() - 1;
        Ok(id)
    }

    /// Split the focused pane of the client's active tab. The new shell runs in
    /// the tab's profile and starts in the focused shell's directory.
    fn split(&mut self, client: u64, axis: Axis, fallback_cwd: String) -> Result<()> {
        let new = self.next_pane;
        let (name, (focused, profile, rect)) = self.with_tab(client, |t, area| {
            let mut root = t.root.clone();
            root.split(t.focused, new, axis);
            let rect = root.geometry(area).into_iter().find(|(id, _)| *id == new).map(|(_, r)| r).unwrap_or(area);
            (t.focused, t.profile.clone(), rect)
        })?;
        let cwd = self
            .panes
            .get(&focused)
            .and_then(|p| p.child.process_id())
            .and_then(pid_cwd)
            .unwrap_or(fallback_cwd);
        let (id, _) = self.spawn_pane(&name, profile.as_deref(), &cwd, rect.rows, rect.cols)?;
        self.with_tab(client, |t, _| {
            t.root.split(focused, id, axis);
            t.focused = id;
            t.zoomed = false;
        })?;
        self.relayout(&name, false)?;
        self.send_snapshot(id);
        Ok(())
    }

    fn kill_pane(&mut self, id: u32) {
        if let Some(p) = self.panes.get_mut(&id) {
            let _ = p.child.kill();
        }
    }

    /// Remove an exited pane, collapsing its tab's layout (or closing the tab).
    /// Returns true when the daemon has nothing left.
    fn remove_pane(&mut self, id: u32) -> bool {
        let Some(mut pane) = self.panes.remove(&id) else { return false };
        let _ = pane.child.try_wait();
        let name = pane.session;
        let Some(s) = self.sessions.iter_mut().find(|s| s.name == name) else { return self.sessions.is_empty() };
        if let Some(ti) = s.tabs.iter().position(|t| t.root.contains(id)) {
            let t = &mut s.tabs[ti];
            match t.root.remove(id) {
                Some(next) => {
                    if t.focused == id {
                        t.focused = next;
                        t.zoomed = false;
                    }
                    if matches!(t.root, Node::Leaf(_)) {
                        t.zoomed = false;
                    }
                }
                None => {
                    s.tabs.remove(ti);
                    if ti < s.active {
                        s.active -= 1;
                    }
                    s.active = s.active.min(s.tabs.len().saturating_sub(1));
                }
            }
        }
        if s.tabs.is_empty() {
            self.sessions.retain(|s| s.name != name);
            let gone: Vec<u64> = self.clients.iter().filter(|(_, c)| c.session == name).map(|(k, _)| *k).collect();
            for k in gone {
                if let Some(c) = self.clients.remove(&k) {
                    let _ = c.tx.send(ServerMsg::Exit(format!("session '{name}' ended")));
                }
            }
        } else {
            let _ = self.relayout(&name, false);
        }
        self.sessions.is_empty()
    }

    fn select(&mut self, id: u64, f: impl Fn(usize, usize) -> usize) -> Result<()> {
        let name = self.client_session(id)?;
        let s = self.session_mut(&name)?;
        if !s.tabs.is_empty() {
            s.active = f(s.active, s.tabs.len());
        }
        self.send_layout(&name);
        Ok(())
    }

    /// Latest attaching/resizing client sets the session size (tmux "latest").
    /// Every client then gets fresh snapshots at that size.
    fn resize(&mut self, name: &str, rows: u16, cols: u16) -> Result<()> {
        let s = self.session_mut(name)?;
        s.rows = rows.max(1);
        s.cols = cols.max(1);
        self.relayout(name, true)
    }

    /// Size every pane's PTY and screen to its rect, then send the layout and
    /// snapshots of resized panes (all panes with `snapshot_all`).
    fn relayout(&mut self, name: &str, snapshot_all: bool) -> Result<()> {
        let s = self.sessions.iter().find(|s| s.name == name).ok_or_else(|| anyhow!("no session named '{name}'"))?;
        let area = s.area();
        let geometry: Vec<_> = s.tabs.iter().flat_map(|t| t.geometry(area)).collect();
        let mut changed = Vec::new();
        for (id, r) in geometry {
            let Some(p) = self.panes.get_mut(&id) else { continue };
            let (rows, cols) = (r.rows.max(1), r.cols.max(1));
            if p.parser.screen().size() != (rows, cols) {
                p.master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })?;
                p.parser.screen_mut().set_size(rows, cols);
                changed.push(id);
            } else if snapshot_all {
                changed.push(id);
            }
        }
        self.send_layout(name);
        for id in changed {
            self.send_snapshot(id);
        }
        Ok(())
    }

    /// Send a pane's full screen to every client of its session.
    fn send_snapshot(&self, id: u32) {
        let Some(p) = self.panes.get(&id) else { return };
        let screen = p.parser.screen();
        let (rows, cols) = screen.size();
        let data = screen.state_formatted();
        self.send_to_session(&p.session, || ServerMsg::Snapshot { pane: id, rows, cols, data: data.clone() });
    }

    fn send_layout(&self, name: &str) {
        let Some(s) = self.sessions.iter().find(|s| s.name == name) else { return };
        let area = s.area();
        let layout = Layout {
            session: s.name.clone(),
            active: s.active,
            tabs: s
                .tabs
                .iter()
                .map(|t| TabInfo {
                    profile: t.profile.clone().unwrap_or_else(|| profile::SYSTEM.into()),
                    color: t.color.clone(),
                    panes: t.geometry(area).into_iter().map(|(id, rect)| PaneInfo { id, rect }).collect(),
                    focused: t.focused,
                    zoomed: t.zoomed,
                })
                .collect(),
        };
        self.send_to_session(name, || ServerMsg::Layout(layout.clone()));
    }

    fn send_to_session(&self, name: &str, msg: impl Fn() -> ServerMsg) {
        for c in self.clients.values().filter(|c| c.session == name) {
            let _ = c.tx.send(msg());
        }
    }

    fn client_session(&self, id: u64) -> Result<String> {
        Ok(self.clients.get(&id).ok_or_else(|| anyhow!("not attached"))?.session.clone())
    }

    fn session_mut(&mut self, name: &str) -> Result<&mut Session> {
        self.sessions.iter_mut().find(|s| s.name == name).ok_or_else(|| anyhow!("no session named '{name}'"))
    }

    /// Run `f` on the client's active tab (with the session's pane area).
    /// Returns the session name and `f`'s result.
    fn with_tab<R>(&mut self, client: u64, f: impl FnOnce(&mut Tab, Rect) -> R) -> Result<(String, R)> {
        let name = self.client_session(client)?;
        let s = self.session_mut(&name)?;
        let area = s.area();
        let tab = s.tabs.get_mut(s.active).ok_or_else(|| anyhow!("no tabs"))?;
        Ok((name, f(tab, area)))
    }

    fn focused_pane(&mut self, client: u64) -> Result<u32> {
        Ok(self.with_tab(client, |t, _| t.focused)?.1)
    }
}

/// Current directory of process `pid`, so a split starts where the focused shell is.
#[cfg(target_os = "macos")]
fn pid_cwd(pid: u32) -> Option<String> {
    let size = std::mem::size_of::<libc::proc_vnodepathinfo>() as libc::c_int;
    // SAFETY: proc_vnodepathinfo is plain data; zeroed is a valid value.
    let mut info: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
    // SAFETY: the buffer is a writable proc_vnodepathinfo of `size` bytes.
    let n = unsafe {
        libc::proc_pidinfo(pid as libc::c_int, libc::PROC_PIDVNODEPATHINFO, 0, (&raw mut info).cast(), size)
    };
    if n != size {
        return None;
    }
    let path: Vec<u8> = info.pvi_cdir.vip_path.as_flattened().iter().take_while(|&&c| c != 0).map(|&c| c as u8).collect();
    String::from_utf8(path).ok().filter(|p| !p.is_empty())
}

#[cfg(not(target_os = "macos"))]
fn pid_cwd(pid: u32) -> Option<String> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok().map(|p| p.to_string_lossy().into_owned())
}
