//! Client ⇄ daemon messages over a unix socket: u32 length prefix + postcard.

use crate::layout::{Dir, Rect};
use crate::workspace::WorkspaceSpec;
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::path::PathBuf;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

const MAX_FRAME: usize = 16 * 1024 * 1024;

pub fn socket_path() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("OUI_SOCKET") {
        return Ok(PathBuf::from(p));
    }
    Ok(crate::profile::oui_root()?.join("oui.sock"))
}

#[derive(Serialize, Deserialize, Debug)]
pub enum ClientMsg {
    /// Attach to `session` (or the most recent one). With `create`, a missing
    /// session is created with a first tab in `profile`.
    Attach { session: Option<String>, create: bool, profile: Option<String>, cwd: String, rows: u16, cols: u16 },
    Input(#[serde(with = "serde_bytes")] Vec<u8>),
    Resize { rows: u16, cols: u16 },
    NewTab { profile: Option<String>, cwd: String },
    SelectTab(usize),
    NextTab,
    PrevTab,
    /// Close every pane of the active tab.
    CloseTab,
    /// Split the focused pane (right, or `down`); `cwd` is a fallback if the
    /// focused shell's directory can't be read.
    Split { down: bool, cwd: String },
    Focus(Dir),
    /// Move the divider nearest the focused pane.
    ResizePane(Dir),
    /// Toggle zoom: the focused pane fills the tab.
    Zoom,
    ClosePane,
    Detach,
    ListSessions,
    KillSession(String),
    KillServer,
    /// Create the workspace's session (unless one with its name is running),
    /// then attach like `Attach`.
    OpenWorkspace { spec: WorkspaceSpec, rows: u16, cols: u16 },
    /// Reply with `ServerMsg::Workspace`: the session's tabs, profiles,
    /// split tree and each shell's current directory.
    DescribeSession(String),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PaneInfo {
    pub id: u32,
    /// Position inside the pane area (below the tab bar).
    pub rect: Rect,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TabInfo {
    pub profile: String,
    pub color: String,
    /// Every pane of the tab; when zoomed only `focused` is visible (full area).
    pub panes: Vec<PaneInfo>,
    pub focused: u32,
    pub zoomed: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Layout {
    pub session: String,
    pub tabs: Vec<TabInfo>,
    pub active: usize,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct SessionInfo {
    pub name: String,
    pub tabs: usize,
    pub clients: usize,
}

#[derive(Serialize, Deserialize, Debug)]
pub enum ServerMsg {
    Layout(Layout),
    /// Full screen state of a pane; client resets its emulator to this.
    Snapshot { pane: u32, rows: u16, cols: u16, #[serde(with = "serde_bytes")] data: Vec<u8> },
    Output { pane: u32, #[serde(with = "serde_bytes")] data: Vec<u8> },
    Sessions(Vec<SessionInfo>),
    Error(String),
    /// Client should exit (detached, session ended, server killed).
    Exit(String),
    Ok,
    Workspace(WorkspaceSpec),
}

pub async fn write_msg<W: AsyncWrite + Unpin, T: Serialize>(w: &mut W, msg: &T) -> Result<()> {
    let buf = postcard::to_stdvec(msg)?;
    w.write_u32(buf.len() as u32).await?;
    w.write_all(&buf).await?;
    Ok(())
}

pub async fn read_msg<R: AsyncRead + Unpin, T: DeserializeOwned>(r: &mut R) -> Result<T> {
    let len = r.read_u32().await? as usize;
    if len > MAX_FRAME {
        bail!("frame too large: {len}");
    }
    let mut buf = vec![0; len];
    r.read_exact(&mut buf).await?;
    Ok(postcard::from_bytes(&buf)?)
}
