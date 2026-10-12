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
use std::time::{Duration, Instant};

const UP: &[u8] = b"\x1b[A";
const DOWN: &[u8] = b"\x1b[B";

/// How long after a focus/resize action the arrows keep working without the
/// prefix (like tmux `bind -r` / `repeat-time`).
pub const REPEAT: Duration = Duration::from_millis(600);

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

/// Length of the key at the start of `bytes`: a whole CSI (`ESC [ … final`)
/// or SS3 (`ESC O x`) sequence, Alt+key (`ESC x`, `ESC ESC [ …`), a UTF-8
/// character, or one byte. None when `bytes` ends inside an escape sequence.
pub fn key_len(bytes: &[u8]) -> Option<usize> {
    match bytes {
        [] => Some(0),
        [0x1b] => Some(1),
        [0x1b, b'[', rest @ ..] => rest.iter().position(|b| (0x40..=0x7e).contains(b)).map(|i| i + 3),
        [0x1b, b'O'] => None,
        [0x1b, b'O', ..] => Some(3),
        [0x1b, 0x1b, b'[' | b'O', ..] => key_len(&bytes[1..]).map(|n| n + 1),
        [0x1b, rest @ ..] => Some(1 + utf8_len(rest)),
        _ => Some(utf8_len(bytes)),
    }
}

/// Bytes of the UTF-8 character starting `bytes` (1 for anything invalid).
fn utf8_len(bytes: &[u8]) -> usize {
    let n = match bytes[0] {
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => 1,
    };
    n.min(bytes.len())
}

/// An arrow key with any modifiers: (`A`-`D`, Shift held). Covers plain
/// (`ESC [ A`, `ESC O A`), xterm-modified (`ESC [ 1 ; 5 A` = Ctrl, `;2` Shift,
/// `;3` Alt, …), Alt as an ESC prefix (`ESC ESC [ A`) and rxvt's Shift
/// (`ESC [ a`) / Ctrl (`ESC O a`) forms.
pub fn arrow(key: &[u8]) -> Option<(u8, bool)> {
    match key {
        [0x1b, b'[' | b'O', c @ b'A'..=b'D'] => Some((*c, false)),
        [0x1b, b'[', c @ b'a'..=b'd'] => Some((c.to_ascii_uppercase(), true)),
        [0x1b, b'O', c @ b'a'..=b'd'] => Some((c.to_ascii_uppercase(), false)),
        [0x1b, 0x1b, rest @ ..] => arrow(&key[1..]).filter(|_| rest.len() == 2),
        [0x1b, b'[', params @ .., c @ b'A'..=b'D'] => {
            let params = std::str::from_utf8(params).ok()?;
            let m: u8 = params.rsplit(';').next()?.parse().ok()?;
            Some((*c, m >= 1 && (m - 1) & 1 == 1))
        }
        _ => None,
    }
}

/// A key for messages: printable as-is, control bytes as `^X`, ESC as `^[`.
pub fn show_key(key: &[u8]) -> String {
    let mut s = String::new();
    for c in String::from_utf8_lossy(key).chars() {
        match c {
            '\x1b' => s.push_str("^["),
            c if (c as u32) < 0x20 => s.push_str(&format!("^{}", (c as u8 + b'@') as char)),
            '\x7f' => s.push_str("^?"),
            c => s.push(c),
        }
    }
    s
}

fn repeatable(a: Action) -> bool {
    matches!(
        a,
        Action::FocusLeft | Action::FocusRight | Action::FocusUp | Action::FocusDown
            | Action::ResizeLeft | Action::ResizeRight | Action::ResizeUp | Action::ResizeDown
    )
}

/// What a key pressed after the prefix means.
enum Resolved {
    Bound(Action, usize),
    Unknown(Vec<u8>, usize),
    /// The input ends inside an escape sequence: wait for the rest.
    Incomplete,
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
    /// Start of a mouse report or escape sequence split across reads.
    pending: Vec<u8>,
    /// Until then, arrows (and other non-letter focus/resize keys) work
    /// without the prefix again: set by a focus or resize action.
    pub repeat_until: Option<Instant>,
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
            repeat_until: None,
        }
    }

    /// The repeat window (focus/resize keys without the prefix) is open.
    pub fn repeating(&self) -> bool {
        self.repeat_until.is_some_and(|t| t > Instant::now())
    }

    /// Open the new-tab profile picker (`new-tab`, or a click on `+`).
    pub fn open_new_tab_picker(&mut self) {
        let opts = picker_options();
        let sel = self.default_profile.as_ref().and_then(|d| opts.iter().position(|(n, _)| n == d));
        self.mode = Mode::Picker(sel.unwrap_or(0));
    }

    /// Resolve the key at the start of `bytes` (input after the prefix). An
    /// explicit binding wins; otherwise an arrow with modifiers acts like the
    /// plain arrow (Ctrl/Alt: whatever the arrow is bound to, i.e. focus), or
    /// resizes when Shift is held.
    fn resolve(&self, bytes: &[u8]) -> Resolved {
        let Some(n) = key_len(bytes) else { return Resolved::Incomplete };
        let key = &bytes[..n];
        if let Some(a) = self.keys.bound(key) {
            return Resolved::Bound(a, n);
        }
        if let Some((c, shift)) = arrow(key) {
            let a = match (shift, c) {
                (true, b'A') => Some(Action::ResizeUp),
                (true, b'B') => Some(Action::ResizeDown),
                (true, b'C') => Some(Action::ResizeRight),
                (true, b'D') => Some(Action::ResizeLeft),
                _ => self.keys.bound(&[0x1b, b'[', c]),
            };
            if let Some(a) = a {
                return Resolved::Bound(a, n);
            }
        }
        Resolved::Unknown(key.to_vec(), n)
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
            // Repeat window after a focus/resize: arrows (any modifier) and
            // H/J/K/L act again without the prefix. Lowercase letters never
            // repeat, so typing right after `prefix l` isn't eaten.
            if matches!(self.mode, Mode::Normal) && self.repeat_until.is_some() {
                if self.repeating() {
                    match self.resolve(&bytes[i..]) {
                        Resolved::Incomplete if bytes[i] == 0x1b && bytes.len() - i > 1 => {
                            self.pending = bytes[i..].to_vec();
                            break;
                        }
                        Resolved::Bound(a, n) if repeatable(a) && !bytes[i].is_ascii_lowercase() => {
                            if !pass.is_empty() {
                                out.push(ClientMsg::Input(std::mem::take(&mut pass)));
                            }
                            i += n;
                            out.extend(self.apply(a));
                            continue;
                        }
                        _ => {}
                    }
                }
                self.repeat_until = None;
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
                    match self.resolve(&bytes[i..]) {
                        // ESC [ 1 ; 5 split across reads: keep the prefix and wait.
                        Resolved::Incomplete => {
                            self.pending = bytes[i..].to_vec();
                            break;
                        }
                        Resolved::Bound(action, n) => {
                            self.mode = Mode::Normal;
                            i += n;
                            out.extend(self.apply(action));
                        }
                        // Unbound key: swallow all of it (a whole escape
                        // sequence, never a stray "[1;5D" in the pane).
                        Resolved::Unknown(key, n) => {
                            self.mode = Mode::Normal;
                            i += n;
                            if key != b"\x1b" {
                                let p = super::render::display_key(&self.keys.prefix.notation);
                                self.flash = Some(format!("unknown key: {p} {} (ignored)", show_key(&key)));
                            }
                        }
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
        if repeatable(action) {
            self.repeat_until = Some(Instant::now() + REPEAT);
        }
        Some(match action {
            Action::NewTab => {
                self.open_new_tab_picker();
                return None;
            }
            Action::NextTab => ClientMsg::NextTab,
            Action::PrevTab => ClientMsg::PrevTab,
            Action::SelectTab(n) => ClientMsg::SelectTab(usize::from(n).saturating_sub(1)),
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
            // Overlays: the keys that follow belong to them. Detach and
            // closing ask first when they'd end the client or kill a running
            // program (the app decides; see `App::on_action`).
            Action::FocusPanel | Action::RenameTab | Action::SessionPicker | Action::Detach | Action::ClosePane | Action::CloseTab => {
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
    fn modified_arrows_after_prefix() {
        let mut inp = Input::new("/tmp".into(), Keys::default(), None);
        // Ctrl still held after the prefix (xterm form), Alt (both forms), rxvt Ctrl.
        for (seq, want) in [
            (&b"\x1b[1;5D"[..], "Focus(Left)"),
            (b"\x1b[1;3A", "Focus(Up)"),
            (b"\x1b\x1b[B", "Focus(Down)"),
            (b"\x1bOc", "Focus(Right)"),
            (b"\x1b[1;2C", "ResizePane(Right)"),
            (b"\x1b[1;6A", "ResizePane(Up)"),
            (b"\x1b[a", "ResizePane(Up)"),
        ] {
            inp.repeat_until = None;
            let out = inp.feed(&[b"\x02", seq].concat());
            assert_eq!(dbg(&out), [want], "{seq:?}");
            assert!(matches!(inp.mode, Mode::Normal));
        }
        // An explicit binding for a modified arrow wins.
        let c = Config::parse("[keys.bindings]\nC-Left = \"zoom\"", None).unwrap();
        let mut inp = Input::new("/tmp".into(), c.keys, None);
        assert_eq!(dbg(&inp.feed(b"\x02\x1b[1;5D")), ["Zoom"]);
        // Arrows unbound: Ctrl-arrow is unbound too (swallowed).
        let c = Config::parse("[keys.bindings]\nLeft = \"none\"", None).unwrap();
        let mut inp = Input::new("/tmp".into(), c.keys, None);
        assert!(inp.feed(b"\x02\x1b[1;5D").is_empty());
    }

    #[test]
    fn unknown_keys_after_prefix_are_swallowed_whole() {
        let mut inp = Input::new("/tmp".into(), Keys::default(), None);
        for seq in [&b"\x1b[15;5~"[..], b"\x1b[1;5P", b"\x1bOP", b"\x1by", b"y", "é".as_bytes()] {
            let out = inp.feed(&[b"\x02", seq, b"ok"].concat());
            assert_eq!(dbg(&out), ["Input([111, 107])"], "{seq:?} must not leak");
            assert!(inp.flash.take().is_some_and(|f| f.starts_with("unknown key: ^b ")));
        }
        // Esc after the prefix cancels quietly.
        assert!(inp.feed(b"\x02\x1b").is_empty());
        assert_eq!(inp.flash, None);
        // A sequence split across reads still counts as one key.
        assert!(inp.feed(b"\x02\x1b[1;").is_empty());
        assert_eq!(dbg(&inp.feed(b"5Dls")), ["Focus(Left)", "Input([108, 115])"]);
    }

    #[test]
    fn focus_and_resize_repeat_without_prefix() {
        let mut inp = Input::new("/tmp".into(), Keys::default(), None);
        assert_eq!(dbg(&inp.feed(b"\x02\x1b[D")), ["Focus(Left)"]);
        assert!(inp.repeating());
        // Arrows (any modifier) and H/J/K/L keep working without the prefix.
        assert_eq!(dbg(&inp.feed(b"\x1b[B")), ["Focus(Down)"]);
        assert_eq!(dbg(&inp.feed(b"\x1b[1;5C\x1b[1;2A")), ["Focus(Right)", "ResizePane(Up)"]);
        assert_eq!(dbg(&inp.feed(b"L")), ["ResizePane(Right)"]);
        // Any other key ends the window and goes to the pane.
        assert_eq!(dbg(&inp.feed(b"x\x1b[A")), ["Input([120, 27, 91, 65])"]);
        assert!(!inp.repeating());
        // Lowercase hjkl never repeat: typing right after `prefix l` is the pane's.
        assert_eq!(dbg(&inp.feed(b"\x02lls")), ["Focus(Right)", "Input([108, 115])"]);
        // The window expires.
        assert_eq!(dbg(&inp.feed(b"\x02k")), ["Focus(Up)"]);
        inp.repeat_until = Some(Instant::now() - Duration::from_millis(1));
        assert_eq!(dbg(&inp.feed(b"\x1b[A")), ["Input([27, 91, 65])"]);
        // Non-repeatable actions don't open it.
        inp.feed(b"\x02z");
        assert!(!inp.repeating());
    }

    #[test]
    fn detach_and_close_ask_the_app() {
        let mut inp = Input::new("/tmp".into(), Keys::default(), None);
        for (k, a) in [(b'd', Action::Detach), (b'x', Action::ClosePane), (b'&', Action::CloseTab)] {
            inp.mode = Mode::Normal;
            assert!(inp.feed(&[2, k]).is_empty());
            assert_eq!(std::mem::take(&mut inp.ui), [UiEvent::Action(a)]);
            assert!(matches!(inp.mode, Mode::Overlay));
        }
    }

    #[test]
    fn key_helpers() {
        assert_eq!(key_len(b"\x1b[1;5Dx"), Some(6));
        assert_eq!(key_len(b"\x1b[1;5"), None);
        assert_eq!(key_len(b"\x1bO"), None);
        assert_eq!(key_len(b"\x1bOAx"), Some(3));
        assert_eq!(key_len(b"\x1b\x1b[Ax"), Some(4));
        assert_eq!(key_len(b"\x1bx"), Some(2));
        assert_eq!(key_len("éa".as_bytes()), Some(2));
        assert_eq!(arrow(b"\x1b[1;5D"), Some((b'D', false)));
        assert_eq!(arrow(b"\x1b[1;4D"), Some((b'D', true)));
        assert_eq!(arrow(b"\x1b[5~"), None);
        assert_eq!(show_key(b"\x1b[1;5P"), "^[[1;5P");
        assert_eq!(show_key(b"\x01"), "^A");
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
