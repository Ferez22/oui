//! Scrollback transfer: the daemon serialises a pane's history plus its
//! screen into one byte stream that, fed to a fresh `vt100::Parser` of the
//! same size (with enough scrollback), reproduces both.
//!
//! History rows are written oldest first, each followed by CRLF (or relying on
//! autowrap for wrapped rows, so copied text joins them), then the screen is
//! scrolled up until every history row has left it, then the regular
//! `state_formatted` snapshot redraws the visible screen.

use std::fmt::Write as _;
use vt100::{Color, Screen};

/// Number of scrollback rows currently held by `screen`.
pub fn history_len(screen: &mut Screen) -> usize {
    let off = screen.scrollback();
    screen.set_scrollback(usize::MAX);
    let len = screen.scrollback();
    screen.set_scrollback(off);
    len
}

/// History rows + screen as bytes; returns (history rows, bytes).
pub fn replay(screen: &mut Screen) -> (usize, Vec<u8>) {
    let off = screen.scrollback();
    let total = history_len(screen);
    let (rows, cols) = screen.size();
    let mut out = Vec::new();
    // Visit history oldest first, one screenful per scroll offset.
    let mut line = 0;
    while line < total {
        screen.set_scrollback(total - line);
        let n = (total - line).min(usize::from(rows));
        for r in 0..n as u16 {
            let last = line + usize::from(r) + 1 == total;
            write_row(screen, r, cols, !last, &mut out);
        }
        line += n;
    }
    if total > 0 {
        out.extend_from_slice(b"\x1b[m");
        out.extend(std::iter::repeat_n(b'\n', usize::from(rows.saturating_sub(1))));
    }
    screen.set_scrollback(0);
    out.extend(screen.state_formatted());
    screen.set_scrollback(off);
    (total, out)
}

#[derive(PartialEq, Clone, Copy)]
struct Attrs {
    fg: Color,
    bg: Color,
    bold: bool,
    dim: bool,
    italic: bool,
    underline: bool,
    inverse: bool,
}

impl Default for Attrs {
    fn default() -> Self {
        Attrs { fg: Color::Default, bg: Color::Default, bold: false, dim: false, italic: false, underline: false, inverse: false }
    }
}

fn sgr(a: &Attrs, out: &mut Vec<u8>) {
    let mut s = String::from("\x1b[0");
    for (on, code) in [(a.bold, 1), (a.dim, 2), (a.italic, 3), (a.underline, 4), (a.inverse, 7)] {
        if on {
            let _ = write!(s, ";{code}");
        }
    }
    for (c, base) in [(a.fg, 30u16), (a.bg, 40)] {
        match c {
            Color::Default => {}
            Color::Idx(i) if i < 8 => {
                let _ = write!(s, ";{}", base + u16::from(i));
            }
            Color::Idx(i) if i < 16 => {
                let _ = write!(s, ";{}", base + 60 + u16::from(i - 8));
            }
            Color::Idx(i) => {
                let _ = write!(s, ";{};5;{i}", base + 8);
            }
            Color::Rgb(r, g, b) => {
                let _ = write!(s, ";{};2;{r};{g};{b}", base + 8);
            }
        }
    }
    s.push('m');
    out.extend_from_slice(s.as_bytes());
}

/// One visible row as text + SGR. Wrapped rows (when `may_wrap`) are written
/// to the last column without a line break so the next row autowraps.
fn write_row(screen: &Screen, row: u16, cols: u16, may_wrap: bool, out: &mut Vec<u8>) {
    let wrapped = may_wrap && screen.row_wrapped(row);
    let attrs = |c: &vt100::Cell| Attrs {
        fg: c.fgcolor(),
        bg: c.bgcolor(),
        bold: c.bold(),
        dim: c.dim(),
        italic: c.italic(),
        underline: c.underline(),
        inverse: c.inverse(),
    };
    // Trailing blank default cells are dropped (unless the row wraps).
    let end = if wrapped {
        cols
    } else {
        (0..cols)
            .rev()
            .find(|&c| screen.cell(row, c).is_some_and(|c| c.has_contents() || attrs(c) != Attrs::default()))
            .map_or(0, |c| c + 1)
    };
    let mut cur = Attrs::default();
    out.extend_from_slice(b"\x1b[m");
    let mut col = 0;
    while col < end {
        let Some(cell) = screen.cell(row, col) else { break };
        let a = attrs(cell);
        if a != cur {
            sgr(&a, out);
            cur = a;
        }
        if cell.has_contents() {
            out.extend_from_slice(cell.contents().as_bytes());
            col += if cell.is_wide() { 2 } else { 1 };
        } else {
            out.push(b' ');
            col += 1;
        }
    }
    if !wrapped || col < cols {
        out.extend_from_slice(b"\x1b[m\r\n");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(screen: &mut Screen) -> Vec<String> {
        let total = history_len(screen);
        let (rows, cols) = screen.size();
        let mut v = Vec::new();
        screen.set_scrollback(total);
        v.extend(screen.rows(0, cols).take(total.min(usize::from(rows))));
        let mut line = v.len();
        while line < total {
            screen.set_scrollback(total - line);
            let n = (total - line).min(usize::from(rows));
            v.extend(screen.rows(0, cols).take(n));
            line += n;
        }
        screen.set_scrollback(0);
        v.extend(screen.rows(0, cols));
        v
    }

    #[test]
    fn replay_reproduces_history_and_screen() {
        let mut src = vt100::Parser::new(5, 20, 100);
        for i in 1..=30 {
            src.process(format!("\x1b[3{}mline {i}\x1b[m\r\n", i % 8).as_bytes());
        }
        // A wrapped line and a partly typed prompt.
        src.process(b"abcdefghijklmnopqrstuvwxyz0123\r\n$ ec");
        let (n, data) = replay(src.screen_mut());
        assert_eq!(n, history_len(src.screen_mut()));
        let mut dst = vt100::Parser::new(5, 20, 100);
        dst.process(&data);
        assert_eq!(lines(dst.screen_mut()), lines(src.screen_mut()));
        assert_eq!(dst.screen().cursor_position(), src.screen().cursor_position());
        assert_eq!(dst.screen().contents(), src.screen().contents());
        // Colours survive.
        dst.screen_mut().set_scrollback(n);
        assert_eq!(dst.screen().cell(0, 0).unwrap().fgcolor(), Color::Idx(1));
    }

    #[test]
    fn empty_history() {
        let mut src = vt100::Parser::new(5, 20, 100);
        src.process(b"hi");
        let (n, data) = replay(src.screen_mut());
        assert_eq!(n, 0);
        let mut dst = vt100::Parser::new(5, 20, 100);
        dst.process(&data);
        assert_eq!(history_len(dst.screen_mut()), 0);
        assert_eq!(dst.screen().contents(), "hi");
    }
}
