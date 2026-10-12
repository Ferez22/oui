//! Prefix-key state machine: raw stdin bytes become pane input or oUI actions.
//!
//! Bytes are forwarded to the focused pane untouched, except the prefix key
//! (Ctrl-b); the key after it is looked up in `BINDINGS`.

use super::picker_options;
use crate::layout::Dir;
use crate::protocol::ClientMsg;

pub const PREFIX: u8 = 0x02; // Ctrl-b

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Opens the profile picker.
    NewTab,
    NextTab,
    PrevTab,
    SelectTab(usize),
    CloseTab,
    SplitRight,
    SplitDown,
    Focus(Dir),
    Resize(Dir),
    Zoom,
    ClosePane,
    Detach,
    /// Send a literal prefix byte to the pane.
    SendPrefix,
}

const UP: &[u8] = b"\x1b[A";
const DOWN: &[u8] = b"\x1b[B";
const RIGHT: &[u8] = b"\x1b[C";
const LEFT: &[u8] = b"\x1b[D";

/// Default bindings: key (bytes after the prefix) → action. Arrow keys are
/// matched in their CSI form (`ESC [ A`); SS3 (`ESC O A`) is normalised to it.
pub const BINDINGS: &[(&[u8], Action)] = &[
    (b"c", Action::NewTab),
    (b"n", Action::NextTab),
    (b"p", Action::PrevTab),
    (b"1", Action::SelectTab(0)),
    (b"2", Action::SelectTab(1)),
    (b"3", Action::SelectTab(2)),
    (b"4", Action::SelectTab(3)),
    (b"5", Action::SelectTab(4)),
    (b"6", Action::SelectTab(5)),
    (b"7", Action::SelectTab(6)),
    (b"8", Action::SelectTab(7)),
    (b"9", Action::SelectTab(8)),
    (b"&", Action::CloseTab),
    (b"%", Action::SplitRight),
    (b"|", Action::SplitRight),
    (b"\"", Action::SplitDown),
    (b"-", Action::SplitDown),
    (LEFT, Action::Focus(Dir::Left)),
    (DOWN, Action::Focus(Dir::Down)),
    (UP, Action::Focus(Dir::Up)),
    (RIGHT, Action::Focus(Dir::Right)),
    (b"h", Action::Focus(Dir::Left)),
    (b"j", Action::Focus(Dir::Down)),
    (b"k", Action::Focus(Dir::Up)),
    (b"l", Action::Focus(Dir::Right)),
    (b"H", Action::Resize(Dir::Left)),
    (b"J", Action::Resize(Dir::Down)),
    (b"K", Action::Resize(Dir::Up)),
    (b"L", Action::Resize(Dir::Right)),
    (b"z", Action::Zoom),
    (b"x", Action::ClosePane),
    (b"d", Action::Detach),
    (&[PREFIX], Action::SendPrefix),
];

/// Action bound to `key` (as returned by `next_key`).
pub fn lookup(key: &[u8]) -> Option<Action> {
    BINDINGS.iter().find(|(k, _)| *k == key).map(|(_, a)| *a)
}

/// The key at the start of `bytes`: an arrow escape sequence (normalised to
/// CSI) or a single byte. Returns the key and how many bytes it used.
fn next_key(bytes: &[u8]) -> (Vec<u8>, usize) {
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
}

pub struct Input {
    pub mode: Mode,
    /// Client cwd: start directory for new tabs, fallback for splits.
    cwd: String,
}

impl Input {
    pub fn new(cwd: String) -> Self {
        Input { mode: Mode::Normal, cwd }
    }

    /// Feed raw stdin bytes; returns the messages to send to the daemon.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<ClientMsg> {
        let mut out = Vec::new();
        let mut pass = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            match self.mode {
                Mode::Normal => {
                    if bytes[i] == PREFIX {
                        self.mode = Mode::Prefix;
                    } else {
                        pass.push(bytes[i]);
                    }
                    i += 1;
                }
                Mode::Prefix => {
                    if !pass.is_empty() {
                        out.push(ClientMsg::Input(std::mem::take(&mut pass)));
                    }
                    let (key, n) = next_key(&bytes[i..]);
                    i += n;
                    self.mode = Mode::Normal;
                    if let Some(msg) = lookup(&key).and_then(|a| self.apply(a)) {
                        out.push(msg);
                    }
                }
                Mode::Picker(sel) => {
                    let (key, n) = next_key(&bytes[i..]);
                    i += n;
                    if let Some(msg) = self.picker_key(sel, &key) {
                        out.push(msg);
                    }
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
                self.mode = Mode::Picker(0);
                return None;
            }
            Action::NextTab => ClientMsg::NextTab,
            Action::PrevTab => ClientMsg::PrevTab,
            Action::SelectTab(i) => ClientMsg::SelectTab(i),
            Action::CloseTab => ClientMsg::CloseTab,
            Action::SplitRight => ClientMsg::Split { down: false, cwd },
            Action::SplitDown => ClientMsg::Split { down: true, cwd },
            Action::Focus(d) => ClientMsg::Focus(d),
            Action::Resize(d) => ClientMsg::ResizePane(d),
            Action::Zoom => ClientMsg::Zoom,
            Action::ClosePane => ClientMsg::ClosePane,
            Action::Detach => ClientMsg::Detach,
            Action::SendPrefix => ClientMsg::Input(vec![PREFIX]),
        })
    }

    fn picker_key(&mut self, sel: usize, key: &[u8]) -> Option<ClientMsg> {
        let opts = picker_options();
        let n = opts.len();
        let pick = |idx: usize| ClientMsg::NewTab { profile: (idx > 0).then(|| opts[idx].0.clone()), cwd: self.cwd.clone() };
        let (mode, msg) = match key {
            UP | b"k" => (Mode::Picker((sel + n - 1) % n), None),
            DOWN | b"j" => (Mode::Picker((sel + 1) % n), None),
            b"\x1b" | b"q" | b"\x03" => (Mode::Normal, None),
            b"\r" | b"\n" => (Mode::Normal, Some(pick(sel))),
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

    #[test]
    fn prefix_keys_map_to_messages() {
        let mut inp = Input::new("/tmp".into());
        let out = inp.feed(b"ab\x02%c\x02\x1bOA\x02H\x02\x02");
        let dbg: Vec<String> = out.iter().map(|m| format!("{m:?}")).collect();
        assert_eq!(dbg, [
            r#"Input([97, 98])"#,
            r#"Split { down: false, cwd: "/tmp" }"#,
            r#"Input([99])"#,
            "Focus(Up)",
            "ResizePane(Left)",
            "Input([2])",
        ]);
    }

    #[test]
    fn every_binding_is_reachable() {
        for (key, action) in BINDINGS {
            assert_eq!(next_key(key).1, key.len(), "{key:?}");
            assert_eq!(lookup(key), Some(*action));
        }
    }
}
