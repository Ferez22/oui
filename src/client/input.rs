//! Prefix-key state machine: raw stdin bytes become pane input or oUI actions.
//!
//! Bytes are forwarded to the focused pane untouched, except the prefix key
//! (`keys.prefix`, default Ctrl-b); the key after it is looked up in
//! `keys.bindings` (see `config::Keys`).

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

pub enum Mode {
    Normal,
    Prefix,
    /// New-tab profile picker; index into `picker_options`.
    Picker(usize),
    /// A client overlay (right panel focus, session picker, rename prompt) has
    /// the keyboard; keys go to `Input::ui` for the app to handle.
    Overlay,
}

/// Something for the app (not the daemon) to handle, in input order.
#[derive(Debug, PartialEq)]
pub enum UiEvent {
    /// A client-side action (panel, session picker, rename).
    Action(Action),
    /// A key pressed while in `Mode::Overlay`.
    Key(Vec<u8>),
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
}

impl Input {
    pub fn new(cwd: String, keys: Keys, default_profile: Option<String>) -> Self {
        Input { mode: Mode::Normal, keys, flash: None, ui: Vec::new(), cwd, default_profile }
    }

    /// Feed raw stdin bytes; returns the messages to send to the daemon.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<ClientMsg> {
        let mut out = Vec::new();
        let mut pass = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
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
            // Not implemented yet: give each its own arm when it lands.
            Action::CopyMode | Action::CommandPalette => {
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
}
