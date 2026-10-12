//! Mouse: SGR mouse reports from the host terminal (`general.mouse`).
//!
//! Wheel scrolls a pane's history (copy mode, left again at the bottom),
//! click focuses a pane or selects a tab, drag selects and copies on
//! release. A pane whose app turned on mouse reporting (vim `mouse=a`, htop)
//! gets the events instead, re-encoded relative to the pane in the protocol
//! and encoding the app asked for.
//!
//! Hold Shift to use the host terminal's own selection: most terminals
//! (iTerm2, Terminal.app, kitty, WezTerm, Alacritty, GNOME, xterm) bypass
//! mouse reporting while Shift is held (Option/Alt on some macOS terminals).

use super::App;
use super::input::Mode;
use crate::protocol::ClientMsg;
use vt100::{MouseProtocolEncoding, MouseProtocolMode};

/// Host terminal: report presses, releases, drags; SGR encoding.
pub const ENABLE: &str = "\x1b[?1000h\x1b[?1002h\x1b[?1006h";
pub const DISABLE: &str = "\x1b[?1006l\x1b[?1002l\x1b[?1000l";

/// One decoded SGR report. `x`/`y` are 0-based screen cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseEvent {
    /// Raw button code: low bits button (0-2, 3 = none), +4 shift, +8 meta,
    /// +16 ctrl, +32 motion, +64 wheel.
    pub code: u16,
    pub x: u16,
    pub y: u16,
    /// `M` (press / motion) vs `m` (release).
    pub press: bool,
}

impl MouseEvent {
    pub fn button(&self) -> u16 {
        self.code & 3
    }
    pub fn motion(&self) -> bool {
        self.code & 32 != 0
    }
    /// Some(true) = wheel up, Some(false) = wheel down.
    pub fn wheel(&self) -> Option<bool> {
        (self.code & 64 != 0 && self.code & 2 == 0).then_some(self.code & 1 == 0)
    }
    fn left_press(&self) -> bool {
        self.press && !self.motion() && self.wheel().is_none() && self.button() == 0
    }
}

pub enum Parsed {
    Event(MouseEvent, usize),
    /// A valid start of a report that the input ends inside of.
    Incomplete,
    Invalid,
}

/// Parse `ESC [ < code ; x ; y (M|m)` at the start of `bytes`.
pub fn parse_sgr(bytes: &[u8]) -> Parsed {
    let Some(rest) = bytes.strip_prefix(b"\x1b[<") else { return Parsed::Invalid };
    let mut nums = [0u32; 3];
    let mut idx = 0;
    let mut digits = 0;
    for (i, &b) in rest.iter().enumerate() {
        match b {
            b'0'..=b'9' if digits < 5 => {
                nums[idx] = nums[idx] * 10 + u32::from(b - b'0');
                digits += 1;
            }
            b';' if idx < 2 && digits > 0 => {
                idx += 1;
                digits = 0;
            }
            b'M' | b'm' if idx == 2 && digits > 0 => {
                if nums[1] == 0 || nums[2] == 0 {
                    return Parsed::Invalid;
                }
                let ev = MouseEvent {
                    code: nums[0].min(255) as u16,
                    x: (nums[1] - 1).min(u32::from(u16::MAX)) as u16,
                    y: (nums[2] - 1).min(u32::from(u16::MAX)) as u16,
                    press: b == b'M',
                };
                return Parsed::Event(ev, 3 + i + 1);
            }
            _ => return Parsed::Invalid,
        }
    }
    Parsed::Incomplete
}

/// Encode `ev` for an app that asked for `mode`/`enc`, at pane-relative
/// cell (`x`, `y`). None when the mode doesn't report this event.
pub fn encode(ev: &MouseEvent, x: u16, y: u16, mode: MouseProtocolMode, enc: MouseProtocolEncoding) -> Option<Vec<u8>> {
    let wheel = ev.wheel().is_some();
    let wanted = match mode {
        MouseProtocolMode::None => false,
        MouseProtocolMode::Press => ev.press && !ev.motion(),
        MouseProtocolMode::PressRelease => !ev.motion(),
        MouseProtocolMode::ButtonMotion => !ev.motion() || ev.button() != 3,
        MouseProtocolMode::AnyMotion => true,
    };
    if !wanted || (wheel && !ev.press) {
        return None;
    }
    match enc {
        MouseProtocolEncoding::Sgr => {
            Some(format!("\x1b[<{};{};{}{}", ev.code, x + 1, y + 1, if ev.press { 'M' } else { 'm' }).into_bytes())
        }
        MouseProtocolEncoding::Default | MouseProtocolEncoding::Utf8 => {
            // Legacy: a release is button 3 (modifiers kept).
            let code = if ev.press { ev.code } else { (ev.code & !3) | 3 };
            let mut out = b"\x1b[M".to_vec();
            out.push(32 + code as u8);
            for v in [x, y] {
                let v = u32::from(v) + 33;
                if enc == MouseProtocolEncoding::Default {
                    out.push(u8::try_from(v).ok()?);
                } else {
                    let c = char::from_u32(v).filter(|_| v < 2048)?;
                    out.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes());
                }
            }
            Some(out)
        }
    }
}

/// A drag in progress, captured by the pane it started in.
#[derive(Debug, Clone, Copy)]
pub enum Drag {
    /// Forwarding to the pane's app.
    App(u32),
    /// Selecting text; `moved` once the pointer left the start cell.
    Select { pane: u32, moved: bool },
}

const WHEEL_LINES: isize = 3;

impl App {
    /// Handle one mouse report; returns messages for the daemon.
    pub(super) fn on_mouse(&mut self, ev: MouseEvent) -> Vec<ClientMsg> {
        let mut out = Vec::new();
        if ev.left_press() {
            self.drag = None; // a lost release must not capture the mouse forever
        }
        // Overlays are keyboard-only (`Input::feed` drops reports while one
        // has the keyboard); never let a click reach the panes under one.
        if self.overlay.is_some() {
            return out;
        }
        // The right panel takes the last columns of the pane area: clicks
        // there aren't the panes' (a drag that started in a pane is clamped).
        if self.drag.is_none() && ev.y > 0 && ev.x >= self.term.0.saturating_sub(self.panel_width()) {
            return out;
        }
        let Some(layout) = &self.layout else { return out };
        let Some(tab) = layout.tabs.get(layout.active) else { return out };
        // Panes are drawn below the tab bar (row 0).
        if ev.y == 0 && self.drag.is_none() {
            use super::render::BarHit;
            if ev.left_press() {
                match super::render::bar_hit(layout, self.term.0, ev.x) {
                    Some(BarHit::Tab(i)) => {
                        self.exit_copy();
                        out.push(ClientMsg::SelectTab(i));
                    }
                    Some(BarHit::NewTab) => {
                        self.exit_copy();
                        self.input.open_new_tab_picker();
                    }
                    Some(BarHit::Session) => {
                        self.input.mode = super::input::Mode::Overlay;
                        self.on_action(crate::config::Action::SessionPicker, &mut out);
                    }
                    None => {}
                }
            }
            return out;
        }
        let (x, y) = (ev.x, ev.y.saturating_sub(1));
        let above = ev.y == 0; // dragged onto the tab bar
        let visible = tab.panes.iter().filter(|p| !tab.zoomed || p.id == tab.focused);
        let hit = visible
            .clone()
            .find(|p| x >= p.rect.x && y >= p.rect.y && x < p.rect.x + p.rect.cols && y < p.rect.y + p.rect.rows)
            .map(|p| (p.id, p.rect));
        let focused = tab.focused;
        // A drag stays with the pane it started in (coordinates clamped).
        let captured = match self.drag {
            Some(Drag::App(id) | Drag::Select { pane: id, .. }) => tab.panes.iter().find(|p| p.id == id).map(|p| (id, p.rect)),
            None => None,
        };
        let Some((pane, rect)) = captured.or(hit) else { return out };
        let rx = x.saturating_sub(rect.x).min(rect.cols.saturating_sub(1));
        let ry = y.saturating_sub(rect.y).min(rect.rows.saturating_sub(1));
        let in_copy = self.copy.as_ref().is_some_and(|c| c.pane == pane);
        let Some(screen) = self.screens.get(&pane).map(|p| p.screen()) else { return out };
        let (mode, enc) = (screen.mouse_protocol_mode(), screen.mouse_protocol_encoding());
        let app_mouse = mode != MouseProtocolMode::None && !in_copy;
        let alt = screen.alternate_screen();
        let app_cursor = screen.application_cursor();
        let forward = |out: &mut Vec<ClientMsg>| {
            if let Some(data) = encode(&ev, rx, ry, mode, enc) {
                out.push(ClientMsg::PaneInput { pane, data });
            }
        };

        if let Some(up) = ev.wheel() {
            if !ev.press {
                return out;
            }
            if in_copy {
                self.copy_scroll(if up { WHEEL_LINES } else { -WHEEL_LINES });
            } else if app_mouse {
                forward(&mut out);
            } else if alt {
                // Full-screen app without mouse support: arrow keys.
                let key: &[u8] = match (up, app_cursor) {
                    (true, false) => b"\x1b[A",
                    (true, true) => b"\x1bOA",
                    (false, false) => b"\x1b[B",
                    (false, true) => b"\x1bOB",
                };
                out.push(ClientMsg::PaneInput { pane, data: key.repeat(WHEEL_LINES as usize) });
            } else if up {
                if self.copy.is_some() {
                    self.exit_copy();
                }
                if pane != focused {
                    out.push(ClientMsg::FocusPane(pane));
                }
                self.enter_copy(pane, true);
                self.copy_scroll(WHEEL_LINES);
            }
            return out;
        }

        if ev.left_press() {
            if self.copy.as_ref().is_some_and(|c| c.pane != pane) {
                self.exit_copy();
            }
            if pane != focused {
                out.push(ClientMsg::FocusPane(pane));
            }
            if app_mouse {
                forward(&mut out);
                self.drag = Some(Drag::App(pane));
            } else {
                if let Some(c) = &mut self.copy {
                    c.sel = None;
                }
                self.drag = Some(Drag::Select { pane, moved: false });
                self.drag_start = (rx, ry);
                if in_copy {
                    self.copy_point(rx, ry);
                }
            }
            return out;
        }

        match (self.drag, ev.press) {
            (Some(Drag::App(_)), true) => forward(&mut out),
            (Some(Drag::App(_)), false) => {
                forward(&mut out);
                self.drag = None;
            }
            (Some(Drag::Select { pane, moved }), true) if ev.motion() => {
                if !moved {
                    if (rx, ry) == self.drag_start {
                        return out;
                    }
                    if self.copy.as_ref().is_none_or(|c| c.pane != pane) {
                        self.enter_copy(pane, true);
                    }
                    let (sx, sy) = self.drag_start;
                    self.copy_point(sx, sy);
                    if let Some(c) = &mut self.copy {
                        c.sel = Some((c.cursor, false));
                    }
                    self.drag = Some(Drag::Select { pane, moved: true });
                }
                // Dragging past the top/bottom edge scrolls.
                if above || y < rect.y {
                    self.copy_scroll(1);
                } else if y >= rect.y + rect.rows {
                    self.copy_scroll(-1);
                }
                self.copy_point(rx, ry);
            }
            (Some(Drag::Select { moved, .. }), false) => {
                self.drag = None;
                if moved {
                    self.copy_key(b"y");
                }
            }
            // Other buttons / stray motion: only apps care.
            _ => {
                if app_mouse {
                    forward(&mut out);
                }
            }
        }
        out
    }

    fn copy_scroll(&mut self, delta: isize) {
        let Some(c) = self.copy.as_mut() else { return };
        let Some(p) = self.screens.get_mut(&c.pane) else { return };
        c.scroll(p, delta);
        if c.mouse && delta < 0 && c.at_bottom(p) && c.sel.is_none() && self.drag.is_none() {
            self.exit_copy();
        }
    }

    /// Move the copy cursor to pane cell (`x`, `y`) of the current view.
    fn copy_point(&mut self, x: u16, y: u16) {
        let Some(c) = self.copy.as_mut() else { return };
        let Some(p) = self.screens.get(&c.pane) else { return };
        c.cursor.line = c.top(p) + usize::from(y);
        c.cursor.col = x;
        if !matches!(self.input.mode, Mode::Copy) {
            self.input.mode = Mode::Copy;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(bytes: &[u8]) -> MouseEvent {
        match parse_sgr(bytes) {
            Parsed::Event(e, n) => {
                assert_eq!(n, bytes.len());
                e
            }
            _ => panic!("no event in {bytes:?}"),
        }
    }

    #[test]
    fn parses_sgr_reports() {
        let e = ev(b"\x1b[<0;10;5M");
        assert_eq!(e, MouseEvent { code: 0, x: 9, y: 4, press: true });
        assert!(e.left_press());
        assert_eq!(ev(b"\x1b[<64;1;1M").wheel(), Some(true));
        assert_eq!(ev(b"\x1b[<65;1;1M").wheel(), Some(false));
        let drag = ev(b"\x1b[<32;3;4M");
        assert!(drag.motion() && drag.button() == 0 && !drag.left_press());
        assert!(!ev(b"\x1b[<0;3;4m").press);
        assert!(matches!(parse_sgr(b"\x1b[<0;10"), Parsed::Incomplete));
        assert!(matches!(parse_sgr(b"\x1b[<"), Parsed::Incomplete));
        assert!(matches!(parse_sgr(b"\x1b[<0;x"), Parsed::Invalid));
        assert!(matches!(parse_sgr(b"\x1b[<0;0;1M"), Parsed::Invalid));
        assert!(matches!(parse_sgr(b"\x1b[A"), Parsed::Invalid));
        // Trailing bytes are left alone.
        assert!(matches!(parse_sgr(b"\x1b[<0;1;1Mabc"), Parsed::Event(_, 9)));
    }

    #[test]
    fn encodes_for_apps() {
        use MouseProtocolEncoding as E;
        use MouseProtocolMode as M;
        let press = ev(b"\x1b[<0;10;5M");
        let release = ev(b"\x1b[<0;10;5m");
        let drag = ev(b"\x1b[<32;10;5M");
        let hover = ev(b"\x1b[<35;10;5M");
        let wheel = ev(b"\x1b[<65;10;5M");
        assert_eq!(encode(&press, 2, 3, M::PressRelease, E::Sgr).unwrap(), b"\x1b[<0;3;4M");
        assert_eq!(encode(&release, 2, 3, M::PressRelease, E::Sgr).unwrap(), b"\x1b[<0;3;4m");
        assert_eq!(encode(&press, 2, 3, M::PressRelease, E::Default).unwrap(), b"\x1b[M #$");
        assert_eq!(encode(&release, 2, 3, M::PressRelease, E::Default).unwrap(), b"\x1b[M##$");
        assert_eq!(encode(&wheel, 0, 0, M::Press, E::Default).unwrap(), b"\x1b[Ma!!");
        assert_eq!(encode(&release, 0, 0, M::Press, E::Sgr), None);
        assert_eq!(encode(&drag, 0, 0, M::PressRelease, E::Sgr), None);
        assert_eq!(encode(&drag, 0, 0, M::ButtonMotion, E::Sgr).unwrap(), b"\x1b[<32;1;1M");
        assert_eq!(encode(&hover, 0, 0, M::ButtonMotion, E::Sgr), None);
        assert_eq!(encode(&hover, 0, 0, M::AnyMotion, E::Sgr).unwrap(), b"\x1b[<35;1;1M");
        assert_eq!(encode(&press, 0, 0, M::None, E::Sgr), None);
        // Legacy encoding can't go past column 222; UTF-8 can.
        assert_eq!(encode(&press, 300, 0, M::PressRelease, E::Default), None);
        assert_eq!(encode(&press, 300, 0, M::PressRelease, E::Utf8).unwrap(), "\x1b[M \u{14d}!".as_bytes());
    }
}
