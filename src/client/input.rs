//! Prefix-key state machine: raw stdin bytes become pane input or oUI actions.
//!
//! Bytes are forwarded to the focused pane untouched, except the prefix key
//! (`keys.prefix`, default Ctrl-b); the key after it is looked up in
//! `keys.bindings` (see `config::Keys`).

use super::mouse::{MouseEvent, Parsed, parse_sgr};
use super::picker_options;
use crate::config::{Action, Keys};
use crate::layout::Dir;
use crate::protocol::ClientMsg;

const UP: &[u8] = b"\x1b[A";
const DOWN: &[u8] = b"\x1b[B";

/// The key at the start of `bytes`: an arrow escape sequence (normalised to
/// CSI) or a single byte. Returns the key and how many bytes it used.
pub fn next_key(bytes: &[u8]) -> (Vec<u8>, usize) {
    match bytes {
        [0x1b, b'[' | b'O', c @ b'A'..=b'D', ..] => (vec![0x1b, b'[', *c], 3),
        [b, ..] => (vec![*b], 1),
        [] => (vec![], 0),
    }
}

/// The key at the start of `bytes` for copy mode: a whole CSI / SS3 escape
/// sequence (PgUp is `ESC [ 5 ~`), or a single byte.
fn next_copy_key(bytes: &[u8]) -> (Vec<u8>, usize) {
    let n = match bytes {
        [0x1b, b'O', _, ..] => 3,
        [0x1b, b'[', rest @ ..] => match rest.iter().position(|b| (0x40..=0x7e).contains(b)) {
            Some(i) => i + 3,
            None => bytes.len(),
        },
        [] => 0,
        _ => 1,
    };
    (bytes[..n].to_vec(), n)
}

pub enum Mode {
    Normal,
    Prefix,
    /// New-tab profile picker; index into `picker_options`.
    Picker(usize),
    /// A client overlay (right panel focus, session picker, rename prompt) has
    /// the keyboard; keys go to `Input::ui` for the app to handle.
    Overlay,
    /// Copy mode: keys go to `copy::Copy` (via `Event::CopyKey`).
    Copy,
}

/// Something for the app (not the daemon) to handle, in input order.
#[derive(Debug, PartialEq)]
pub enum UiEvent {
    /// A client-side action (panel, session picker, rename).
    Action(Action),
    /// A key pressed while in `Mode::Overlay`.
    Key(Vec<u8>),
    /// The prefix was pressed in `Mode::Overlay`: close the overlay (input is
    /// in prefix mode already), like the prefix leaves copy mode.
    Leave,
}

/// Input the client handles itself (needs screens/layout); queued by `feed`
/// in `Input::events` for the caller to drain.
#[derive(Debug, PartialEq)]
pub enum Event {
    Mouse(MouseEvent),
    CopyKey(Vec<u8>),
    EnterCopy,
    ExitCopy,
}

pub struct Input {
    pub mode: Mode,
    pub keys: Keys,
    /// Message for the status bar produced by the last `feed` (e.g. an action
    /// that isn't implemented yet). The caller takes it.
    pub flash: Option<String>,
    /// Client-side events produced by the last `feed`. The caller drains it.
    pub ui: Vec<UiEvent>,
    /// Client cwd: start directory for new tabs, fallback for splits.
    cwd: String,
    /// Profile preselected in the new-tab picker.
    default_profile: Option<String>,
    /// Parse mouse reports (`general.mouse`); set by the caller.
    pub mouse: bool,
    pub events: Vec<Event>,
    /// Start of a mouse report split across reads.
    pending: Vec<u8>,
}

impl Input {
    pub fn new(cwd: String, keys: Keys, default_profile: Option<String>) -> Self {
        Input {
            mode: Mode::Normal,
            keys,
            flash: None,
            ui: Vec::new(),
            cwd,
            default_profile,
            mouse: false,
            events: Vec::new(),
            pending: Vec::new(),
        }
    }

    /// Feed raw stdin bytes; returns the messages to send to the daemon.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<ClientMsg> {
        let joined;
        let bytes = if self.pending.is_empty() {
            bytes
        } else {
            joined = [std::mem::take(&mut self.pending).as_slice(), bytes].concat();
            &joined[..]
        };
        let mut out = Vec::new();
        let mut pass = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            if self.mouse && bytes[i] == 0x1b {
                match parse_sgr(&bytes[i..]) {
                    Parsed::Event(ev, n) => {
                        i += n;
                        match self.mode {
                            // Overlays (new-tab picker, panel focus, session
                            // picker, rename) are keyboard-only: a click must
                            // not reach the panes underneath.
                            Mode::Picker(_) | Mode::Overlay => {}
                            Mode::Prefix => self.mode = Mode::Normal,
                            _ => self.events.push(Event::Mouse(ev)),
                        }
                        continue;
                    }
                    Parsed::Incomplete if bytes.len() - i >= 3 => {
                        self.pending = bytes[i..].to_vec();
                        break;
                    }
                    _ => {}
                }
            }
            match self.mode {
                Mode::Normal => match self.keys.match_prefix(&bytes[i..]) {
                    Some(n) => {
                        self.mode = Mode::Prefix;
                        i += n;
                    }
                    None => {
                        pass.push(bytes[i]);
                        i += 1;
                    }
                },
                Mode::Prefix => {
                    if !pass.is_empty() {
                        out.push(ClientMsg::Input(std::mem::take(&mut pass)));
                    }
                    self.mode = Mode::Normal;
                    match self.keys.lookup(&bytes[i..]) {
                        Some((action, n)) => {
                            i += n;
                            if let Some(msg) = self.apply(action) {
                                out.push(msg);
                            }
                        }
                        // Unbound key: swallow it (a whole arrow sequence, not just ESC).
                        None => i += next_key(&bytes[i..]).1,
                    }
                }
                Mode::Copy => match self.keys.match_prefix(&bytes[i..]) {
                    // The prefix leaves copy mode and works as usual.
                    Some(n) => {
                        self.events.push(Event::ExitCopy);
                        self.mode = Mode::Prefix;
                        i += n;
                    }
                    None => {
                        let (key, n) = next_copy_key(&bytes[i..]);
                        i += n;
                        self.events.push(Event::CopyKey(key));
                    }
                },
                // As in copy mode, the prefix closes the new-tab picker or an
                // overlay and then works as usual.
                Mode::Picker(_) | Mode::Overlay if self.keys.match_prefix(&bytes[i..]).is_some() => {
                    if matches!(self.mode, Mode::Overlay) {
                        self.ui.push(UiEvent::Leave);
                    }
                    self.mode = Mode::Prefix;
                    i += self.keys.match_prefix(&bytes[i..]).unwrap_or(1);
                }
                Mode::Picker(sel) => {
                    let (key, n) = next_key(&bytes[i..]);
                    i += n;
                    if let Some(msg) = self.picker_key(sel, &key) {
                        out.push(msg);
                    }
                }
                Mode::Overlay => {
                    if !pass.is_empty() {
                        out.push(ClientMsg::Input(std::mem::take(&mut pass)));
                    }
                    let (key, n) = next_key(&bytes[i..]);
                    i += n;
                    self.ui.push(UiEvent::Key(key));
                }
            }
        }
        if !pass.is_empty() {
            out.push(ClientMsg::Input(pass));
        }
        out
    }

    /// Turn an action into a daemon message (or a local mode change).
    fn apply(&mut self, action: Action) -> Option<ClientMsg> {
        let cwd = self.cwd.clone();
        Some(match action {
            Action::NewTab => {
                let opts = picker_options();
                let sel = self.default_profile.as_ref().and_then(|d| opts.iter().position(|(n, _)| n == d));
                self.mode = Mode::Picker(sel.unwrap_or(0));
                return None;
            }
            Action::NextTab => ClientMsg::NextTab,
            Action::PrevTab => ClientMsg::PrevTab,
            Action::SelectTab(n) => ClientMsg::SelectTab(usize::from(n).saturating_sub(1)),
            Action::CloseTab => ClientMsg::CloseTab,
            Action::ClosePane => ClientMsg::ClosePane,
            Action::Detach => ClientMsg::Detach,
            Action::SplitRight => ClientMsg::Split { down: false, cwd },
            Action::SplitDown => ClientMsg::Split { down: true, cwd },
            Action::FocusLeft => ClientMsg::Focus(Dir::Left),
            Action::FocusRight => ClientMsg::Focus(Dir::Right),
            Action::FocusUp => ClientMsg::Focus(Dir::Up),
            Action::FocusDown => ClientMsg::Focus(Dir::Down),
            Action::ResizeLeft => ClientMsg::ResizePane(Dir::Left),
            Action::ResizeRight => ClientMsg::ResizePane(Dir::Right),
            Action::ResizeUp => ClientMsg::ResizePane(Dir::Up),
            Action::ResizeDown => ClientMsg::ResizePane(Dir::Down),
            Action::Zoom => ClientMsg::Zoom,
            Action::SendPrefix => ClientMsg::Input(self.keys.prefix.seqs[0].clone()),
            Action::TogglePanel => {
                self.ui.push(UiEvent::Action(action));
                return None;
            }
            // Overlays: the keys that follow belong to them.
            Action::FocusPanel | Action::RenameTab | Action::SessionPicker => {
                self.ui.push(UiEvent::Action(action));
                self.mode = Mode::Overlay;
                return None;
            }
            Action::CopyMode => {
                self.mode = Mode::Copy;
                self.events.push(Event::EnterCopy);
                return None;
            }
            // Not implemented yet: give each its own arm when it lands.
            Action::CommandPalette => {
                self.flash = Some(format!("{action}: not available yet"));
                return None;
            }
        })
    }

    fn picker_key(&mut self, sel: usize, key: &[u8]) -> Option<ClientMsg> {
        let opts = picker_options();
        let n = opts.len();
        let pick = |idx: usize| ClientMsg::NewTab { profile: (idx > 0).then(|| opts[idx].0.clone()), cwd: self.cwd.clone(), cmd: None };
        let (mode, msg) = match key {
            UP | b"k" => (Mode::Picker((sel + n - 1) % n), None),
            DOWN | b"j" => (Mode::Picker((sel + 1) % n), None),
            b"\x1b" | b"q" | b"\x03" => (Mode::Normal, None),
            b"\r" | b"\n" => (Mode::Normal, Some(pick(sel.min(n - 1)))),
            [d @ b'0'..=b'9'] if ((d - b'0') as usize) < n => (Mode::Normal, Some(pick((d - b'0') as usize))),
            _ => (Mode::Picker(sel), None),
        };
        self.mode = mode;
        msg
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn dbg(out: &[ClientMsg]) -> Vec<String> {
        out.iter().map(|m| format!("{m:?}")).collect()
    }

    #[test]
    fn prefix_keys_map_to_messages() {
        let mut inp = Input::new("/tmp".into(), Keys::default(), None);
        let out = inp.feed(b"ab\x02%c\x02\x1bOA\x02H\x02\x02\x023");
        assert_eq!(dbg(&out), [
            r#"Input([97, 98])"#,
            r#"Split { down: false, cwd: "/tmp" }"#,
            r#"Input([99])"#,
            "Focus(Up)",
            "ResizePane(Left)",
            "Input([2])",
            "SelectTab(2)",
        ]);
    }

    #[test]
    fn prefix_split_across_reads() {
        let mut inp = Input::new("/tmp".into(), Keys::default(), None);
        assert!(inp.feed(b"\x02").is_empty());
        assert_eq!(dbg(&inp.feed(b"z")), ["Zoom"]);
    }

    #[test]
    fn custom_keys() {
        let c = Config::parse("[keys]\nprefix = \"C-a\"\n[keys.bindings]\nv = \"split-right\"\n\"%\" = \"none\"", None).unwrap();
        let mut inp = Input::new("/tmp".into(), c.keys, None);
        let out = inp.feed(b"\x02\x01v\x01%\x01\x01");
        assert_eq!(dbg(&out), [
            "Input([2])",
            r#"Split { down: false, cwd: "/tmp" }"#,
            "Input([1])",
        ]);
    }

    #[test]
    fn copy_mode_keys_and_mouse() {
        let mut inp = Input::new("/tmp".into(), Keys::default(), None);
        inp.mouse = true;
        assert!(inp.feed(b"\x02[").is_empty());
        assert!(matches!(inp.mode, Mode::Copy));
        assert!(inp.feed(b"k\x1b[5~\x1b[A/x\r").is_empty());
        let keys: Vec<Event> = ["", "k", "\x1b[5~", "\x1b[A", "/", "x", "\r"]
            .iter()
            .map(|k| if k.is_empty() { Event::EnterCopy } else { Event::CopyKey(k.as_bytes().to_vec()) })
            .collect();
        assert_eq!(std::mem::take(&mut inp.events), keys);
        // A mouse report split across reads, then the prefix leaves copy mode.
        assert!(inp.feed(b"\x1b[<64;5").is_empty());
        assert!(inp.feed(b";6M\x02z").iter().map(|m| format!("{m:?}")).eq(["Zoom".to_string()]));
        assert_eq!(inp.events, [
            Event::Mouse(MouseEvent { code: 64, x: 4, y: 5, press: true }),
            Event::ExitCopy,
        ]);
        assert!(matches!(inp.mode, Mode::Normal));
        // Normal mode: typing around a click still reaches the pane.
        inp.events.clear();
        assert_eq!(dbg(&inp.feed(b"a\x1b[<0;1;2Mb")), ["Input([97, 98])"]);
        assert_eq!(inp.events.len(), 1);
    }

    #[test]
    fn mouse_off_passes_reports_through() {
        let mut inp = Input::new("/tmp".into(), Keys::default(), None);
        assert_eq!(dbg(&inp.feed(b"\x1b[<0;1;2M")).len(), 1);
        assert!(inp.events.is_empty());
    }

    #[test]
    fn unimplemented_actions_flash() {
        let c = Config::parse("[keys.bindings]\nP = \"command-palette\"", None).unwrap();
        let mut inp = Input::new("/tmp".into(), c.keys, None);
        assert!(inp.feed(b"\x02P").is_empty());
        assert_eq!(inp.flash.as_deref(), Some("command-palette: not available yet"));
        assert!(matches!(inp.mode, Mode::Normal));
    }

    #[test]
    fn overlay_actions_capture_keys() {
        let mut inp = Input::new("/tmp".into(), Keys::default(), None);
        // Toggle stays in normal mode: the following bytes go to the pane.
        assert_eq!(dbg(&inp.feed(b"\x02bls")), ["Input([108, 115])"]);
        assert_eq!(inp.ui, [UiEvent::Action(Action::TogglePanel)]);
        inp.ui.clear();
        // Session picker: keys after it (arrows whole) are the overlay's.
        assert_eq!(dbg(&inp.feed(b"ab\x02sj\x1b[A\r")), ["Input([97, 98])"]);
        assert_eq!(inp.ui, [
            UiEvent::Action(Action::SessionPicker),
            UiEvent::Key(b"j".to_vec()),
            UiEvent::Key(b"\x1b[A".to_vec()),
            UiEvent::Key(b"\r".to_vec()),
        ]);
        assert!(matches!(inp.mode, Mode::Overlay));
        inp.ui.clear();
        inp.mode = Mode::Normal;
        assert!(inp.feed(b"\x02\t").is_empty());
        assert_eq!(inp.ui, [UiEvent::Action(Action::FocusPanel)]);
    }

    #[test]
    fn overlays_drop_mouse_and_prefix_leaves() {
        let mut inp = Input::new("/tmp".into(), Keys::default(), None);
        inp.mouse = true;
        assert!(inp.feed(b"\x02s\x1b[<0;5;5Mj").is_empty());
        assert!(inp.events.is_empty(), "no click reaches the panes under an overlay");
        assert_eq!(inp.ui, [UiEvent::Action(Action::SessionPicker), UiEvent::Key(b"j".to_vec())]);
        inp.ui.clear();
        // Like copy mode, the prefix leaves the overlay and works as usual.
        assert_eq!(dbg(&inp.feed(b"\x02z")), ["Zoom"]);
        assert_eq!(inp.ui, [UiEvent::Leave]);
        assert!(matches!(inp.mode, Mode::Normal));
        // New-tab picker: clicks dropped too, the prefix closes it.
        assert!(inp.feed(b"\x02c\x1b[<0;5;5M").is_empty());
        assert!(inp.events.is_empty());
        assert_eq!(dbg(&inp.feed(b"\x02z")), ["Zoom"]);
        assert!(matches!(inp.mode, Mode::Normal));
    }
}
