//! Client ⇄ daemon messages over a unix socket.
//!
//! Every frame is `MAGIC`, `PROTOCOL_VERSION` (u16 BE), the payload length
//! (u32 BE), then the postcard-encoded message. postcard encodes enum variants
//! by index, so a peer built from other sources could silently misread a
//! message (e.g. a new variant as `KillServer`); both sides therefore check
//! the header and refuse anything else.
//!
//! oUI before versioning (protocol 1) sent a bare u32 length first. `MAGIC`
//! read as such a length is ~1.8 GB, over `MAX_FRAME`, so an old daemon drops
//! a new client's connection without acting on it. `oui kill-server` can still
//! stop an old daemon with `LEGACY_KILL_SERVER`, and any daemon from protocol 2
//! on with `KILL_FRAME`, whatever its version.

use crate::layout::{Dir, Rect};
use crate::workspace::WorkspaceSpec;
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::path::PathBuf;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

const MAX_FRAME: usize = 16 * 1024 * 1024;

/// Bump whenever the encoding of `ClientMsg`/`ServerMsg` changes (the golden
/// test in this file fails then). 1 = the unversioned protocol.
pub const PROTOCOL_VERSION: u16 = 2;
/// `oui --version`.
pub const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), " (protocol 2)");

/// Starts every frame.
pub const MAGIC: [u8; 4] = *b"oUIp";
const HEADER: usize = 4 + 2 + 4;
/// A whole frame on its own, understood by every versioned daemon: stop the
/// server. Lets a client stop a daemon whose message encoding it doesn't know.
pub const KILL_FRAME: [u8; 4] = *b"oUIk";
/// `ClientMsg::KillServer` as protocol 1 sent it: length 1, variant index 17.
pub const LEGACY_KILL_SERVER: [u8; 5] = [0, 0, 0, 1, 17];
/// Protocol 1's `ServerMsg::Ok` reply (variant index 6).
pub const LEGACY_OK: [u8; 5] = [0, 0, 0, 1, 6];
/// Protocol 1's `ServerMsg::Exit` variant index.
const LEGACY_EXIT: u8 = 5;

/// The peer speaks another protocol: `None` for the unversioned protocol 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mismatch(pub Option<u16>);

impl std::fmt::Display for Mismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            Some(v) => write!(f, "peer speaks oUI protocol {v}, this oui {VERSION}"),
            None => write!(f, "peer speaks the old unversioned oUI protocol (1), this oui {VERSION}"),
        }
    }
}

impl std::error::Error for Mismatch {}

pub fn socket_path() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("OUI_SOCKET") {
        return Ok(PathBuf::from(p));
    }
    Ok(crate::profile::oui_root()?.join("oui.sock"))
}

#[derive(Serialize, Deserialize, Debug)]
pub enum ClientMsg {
    /// Attach to `session` (or the most recent one). With `create`, a missing
    /// session is created with a first tab in `profile`. Sending it again on an
    /// attached connection switches this client to another session.
    Attach { session: Option<String>, create: bool, profile: Option<String>, cwd: String, rows: u16, cols: u16 },
    Input(#[serde(with = "serde_bytes")] Vec<u8>),
    Resize { rows: u16, cols: u16 },
    /// New tab in `profile`; `cmd` is typed into its shell (plus Enter) right
    /// after it starts (e.g. a tool's login command).
    NewTab { profile: Option<String>, cwd: String, cmd: Option<String> },
    /// Name the active tab (`None` clears it).
    RenameTab(Option<String>),
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
    // --- Scrollback / mouse (copy mode).
    /// Ask for a pane's screen *with* its scrollback; answered with `History`
    /// to this client only.
    History(u32),
    /// Focus a pane of the active tab by id (mouse click).
    FocusPane(u32),
    /// Input for a specific pane of the client's session, focused or not
    /// (mouse events forwarded to an app under the pointer).
    PaneInput { pane: u32, #[serde(with = "serde_bytes")] data: Vec<u8> },
    /// Which programs (other than the shell) run in the foreground of the
    /// focused pane, or of every pane of the active tab with `tab`; answered
    /// with `ServerMsg::Busy` to this client only (close confirmations).
    Busy { tab: bool },
    /// First message of every connection; answered with `ServerMsg::Hello`.
    Hello,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PaneInfo {
    pub id: u32,
    /// Position inside the pane area (below the tab bar).
    pub rect: Rect,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TabInfo {
    /// User-given name (`RenameTab`); the profile is always shown as well.
    pub name: Option<String>,
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

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
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
    // --- Scrollback / clipboard.
    /// Like `Snapshot`, but `data` first replays `history` scrollback lines so
    /// a parser with enough scrollback ends up with the daemon's history too.
    History { pane: u32, rows: u16, cols: u16, history: u32, #[serde(with = "serde_bytes")] data: Vec<u8> },
    /// An app in a pane set the clipboard (OSC 52); base64 payload as sent.
    Clipboard(#[serde(with = "serde_bytes")] Vec<u8>),
    /// Reply to `ClientMsg::Busy`: names of the foreground programs found.
    Busy(Vec<String>),
    /// Reply to `ClientMsg::Hello`: the daemon's `VERSION`.
    Hello(String),
}

/// One frame: header + postcard payload.
pub fn encode<T: Serialize>(msg: &T) -> Result<Vec<u8>> {
    let payload = postcard::to_stdvec(msg)?;
    if payload.len() > MAX_FRAME {
        bail!("message too large: {} bytes", payload.len());
    }
    let mut buf = Vec::with_capacity(HEADER + payload.len());
    buf.extend_from_slice(&MAGIC);
    buf.extend_from_slice(&PROTOCOL_VERSION.to_be_bytes());
    buf.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    buf.extend_from_slice(&payload);
    Ok(buf)
}

/// A whole payload, nothing left over.
pub fn decode<T: DeserializeOwned>(payload: &[u8]) -> Result<T> {
    let (msg, rest) = postcard::take_from_bytes(payload)?;
    if !rest.is_empty() {
        bail!("{} unexpected trailing byte(s) in frame", rest.len());
    }
    Ok(msg)
}

pub async fn write_msg<W: AsyncWrite + Unpin, T: Serialize>(w: &mut W, msg: &T) -> Result<()> {
    w.write_all(&encode(msg)?).await?;
    Ok(())
}

/// What a daemon can receive.
pub enum Frame<T> {
    Msg(T),
    /// `KILL_FRAME`.
    Kill,
}

/// Read one frame. A peer on another protocol version gives a `Mismatch` error.
pub async fn read_frame<R: AsyncRead + Unpin, T: DeserializeOwned>(r: &mut R) -> Result<Frame<T>> {
    let mut magic = [0u8; 4];
    r.read_exact(&mut magic).await?;
    if magic == KILL_FRAME {
        return Ok(Frame::Kill);
    }
    if magic != MAGIC {
        if u32::from_be_bytes(magic) as usize <= MAX_FRAME {
            // A plausible protocol 1 length prefix.
            return Err(Mismatch(None).into());
        }
        bail!("not an oUI frame");
    }
    let version = r.read_u16().await?;
    if version != PROTOCOL_VERSION {
        return Err(Mismatch(Some(version)).into());
    }
    let len = r.read_u32().await? as usize;
    if len > MAX_FRAME {
        bail!("frame too large: {len}");
    }
    let mut buf = vec![0; len];
    r.read_exact(&mut buf).await?;
    Ok(Frame::Msg(decode(&buf)?))
}

pub async fn read_msg<R: AsyncRead + Unpin, T: DeserializeOwned>(r: &mut R) -> Result<T> {
    match read_frame(r).await? {
        Frame::Msg(m) => Ok(m),
        Frame::Kill => bail!("unexpected kill frame"),
    }
}

/// A protocol 1 `ServerMsg::Exit(reason)` frame, so an old client connected
/// to a new daemon says why instead of just losing the connection.
pub fn legacy_exit(reason: &str) -> Vec<u8> {
    let mut payload = vec![LEGACY_EXIT];
    payload.extend(postcard::to_stdvec(reason).unwrap_or_default());
    let mut buf = (payload.len() as u32).to_be_bytes().to_vec();
    buf.extend(payload);
    buf
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{Axis, Tree};
    use crate::workspace::{PaneSpec, TabSpec};

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    /// Pins the wire encoding. If this fails you changed the protocol: bump
    /// `PROTOCOL_VERSION` (and `VERSION`), then update the expected bytes.
    #[test]
    fn golden_encodings() {
        let rect = Rect::new(1, 2, 3, 4);
        let spec = WorkspaceSpec {
            name: "w".into(),
            tabs: vec![TabSpec {
                profile: Some("p".into()),
                layout: Tree::Split {
                    axis: Axis::Vertical,
                    ratio: 0.5,
                    a: Box::new(Tree::Leaf(PaneSpec { cwd: "/".into(), cmd: Some("c".into()) })),
                    b: Box::new(Tree::Leaf(PaneSpec { cwd: "/".into(), cmd: None })),
                },
            }],
        };
        let client: Vec<(ClientMsg, &str)> = vec![
            (
                ClientMsg::Attach { session: Some("s".into()), create: true, profile: None, cwd: "/".into(), rows: 24, cols: 300 },
                "000101730100012f18ac02",
            ),
            (ClientMsg::Input(b"ab".to_vec()), "01026162"),
            (ClientMsg::Resize { rows: 1, cols: 2 }, "020102"),
            (ClientMsg::NewTab { profile: Some("p".into()), cwd: "/".into(), cmd: Some("c".into()) }, "03010170012f010163"),
            (ClientMsg::RenameTab(Some("n".into())), "0401016e"),
            (ClientMsg::SelectTab(3), "0503"),
            (ClientMsg::NextTab, "06"),
            (ClientMsg::PrevTab, "07"),
            (ClientMsg::CloseTab, "08"),
            (ClientMsg::Split { down: true, cwd: "/".into() }, "0901012f"),
            (ClientMsg::Focus(Dir::Up), "0a02"),
            (ClientMsg::ResizePane(Dir::Down), "0b03"),
            (ClientMsg::Zoom, "0c"),
            (ClientMsg::ClosePane, "0d"),
            (ClientMsg::Detach, "0e"),
            (ClientMsg::ListSessions, "0f"),
            (ClientMsg::KillSession("k".into()), "10016b"),
            (ClientMsg::KillServer, "11"),
            (ClientMsg::OpenWorkspace { spec: spec.clone(), rows: 5, cols: 6 }, "1201770101017001010000003f00012f01016300012f000506"),
            (ClientMsg::DescribeSession("d".into()), "130164"),
            (ClientMsg::History(7), "1407"),
            (ClientMsg::FocusPane(8), "1508"),
            (ClientMsg::PaneInput { pane: 9, data: b"x".to_vec() }, "16090178"),
            (ClientMsg::Busy { tab: true }, "1701"),
            (ClientMsg::Hello, "18"),
        ];
        let server: Vec<(ServerMsg, &str)> = vec![
            (
                ServerMsg::Layout(Layout {
                    session: "s".into(),
                    tabs: vec![TabInfo {
                        name: None,
                        profile: "p".into(),
                        color: "c".into(),
                        panes: vec![PaneInfo { id: 1, rect }],
                        focused: 1,
                        zoomed: false,
                    }],
                    active: 0,
                }),
                "000173010001700163010101020304010000",
            ),
            (ServerMsg::Snapshot { pane: 1, rows: 2, cols: 3, data: b"d".to_vec() }, "010102030164"),
            (ServerMsg::Output { pane: 1, data: b"o".to_vec() }, "0201016f"),
            (ServerMsg::Sessions(vec![SessionInfo { name: "s".into(), tabs: 1, clients: 2 }]), "030101730102"),
            (ServerMsg::Error("e".into()), "040165"),
            (ServerMsg::Exit("x".into()), "050178"),
            (ServerMsg::Ok, "06"),
            (ServerMsg::Workspace(spec), "0701770101017001010000003f00012f01016300012f00"),
            (ServerMsg::History { pane: 1, rows: 2, cols: 3, history: 4, data: b"h".to_vec() }, "08010203040168"),
            (ServerMsg::Clipboard(b"c".to_vec()), "090163"),
            (ServerMsg::Busy(vec!["vim".into()]), "0a010376696d"),
            (ServerMsg::Hello(VERSION.into()), ""),
        ];
        for (msg, want) in &client {
            assert_eq!(hex(&postcard::to_stdvec(msg).unwrap()), *want, "{msg:?}");
        }
        for (msg, want) in &server {
            let got = postcard::to_stdvec(msg).unwrap();
            if want.is_empty() {
                assert_eq!(got[0], 0x0b, "{msg:?}");
            } else {
                assert_eq!(hex(&got), *want, "{msg:?}");
            }
        }
        // Protocol 1 numbered these the same; the legacy frames depend on it.
        assert_eq!(LEGACY_KILL_SERVER[4..], postcard::to_stdvec(&ClientMsg::KillServer).unwrap()[..]);
        assert_eq!(LEGACY_OK[4..], postcard::to_stdvec(&ServerMsg::Ok).unwrap()[..]);
        assert_eq!(postcard::to_stdvec(&ServerMsg::Exit(String::new())).unwrap()[0], LEGACY_EXIT);
    }

    #[test]
    fn frame_layout_and_version_string() {
        let f = encode(&ClientMsg::Zoom).unwrap();
        assert_eq!(f, [b'o', b'U', b'I', b'p', 0, PROTOCOL_VERSION as u8, 0, 0, 0, 1, 0x0c]);
        assert!(VERSION.ends_with(&format!(" (protocol {PROTOCOL_VERSION})")));
        assert!(VERSION.starts_with(env!("CARGO_PKG_VERSION")));
        // An unversioned daemon reads the magic as a length and must reject it.
        assert!(u32::from_be_bytes(MAGIC) as usize > MAX_FRAME);
        assert!(u32::from_be_bytes(KILL_FRAME) as usize > MAX_FRAME);
    }

    #[tokio::test]
    async fn read_checks_header_and_trailing_bytes() {
        let read = |bytes: Vec<u8>| async move {
            let mut r = &bytes[..];
            read_msg::<_, ClientMsg>(&mut r).await
        };
        assert!(matches!(read(encode(&ClientMsg::Hello).unwrap()).await, Ok(ClientMsg::Hello)));
        // Other version.
        let mut f = encode(&ClientMsg::Hello).unwrap();
        f[5] = 3;
        let e = read(f).await.unwrap_err();
        assert_eq!(e.downcast_ref::<Mismatch>(), Some(&Mismatch(Some(3))));
        // Protocol 1 frame (bare length).
        let e = read(LEGACY_KILL_SERVER.to_vec()).await.unwrap_err();
        assert_eq!(e.downcast_ref::<Mismatch>(), Some(&Mismatch(None)));
        // Trailing bytes after the message.
        let mut f = encode(&ClientMsg::Zoom).unwrap();
        f[9] = 2;
        f.push(0);
        assert!(read(f).await.unwrap_err().to_string().contains("trailing"));
        // Garbage.
        assert!(read(b"GET / HTTP/1.1\r\n".to_vec()).await.is_err());
        // Kill frame is only for the daemon's read_frame.
        let mut r = &KILL_FRAME[..];
        assert!(matches!(read_frame::<_, ClientMsg>(&mut r).await, Ok(Frame::Kill)));
    }

    #[test]
    fn legacy_exit_frame() {
        let f = legacy_exit("hi");
        assert_eq!(f, [0, 0, 0, 4, 5, 2, b'h', b'i']);
    }
}
