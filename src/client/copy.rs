//! Copy mode: browse a pane's scrollback with vi keys, search, select, yank.
//!
//! Positions are absolute lines in the pane's buffer: `0..hist` is the
//! scrollback (oldest first), `hist..hist + rows` the live screen. New output
//! appends to the history, so absolute lines stay put while the view is
//! scrolled (vt100 keeps its scroll offset anchored the same way). The view
//! itself is the client parser's scrollback offset, so the normal pane
//! renderer draws it; `overlay` adds selection, matches, cursor, position.

use super::clipboard;
use super::input::Mode;
use super::App;
use crate::protocol::ClientMsg;
use crate::scrollback::history_len;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use vt100::Parser;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub struct Pos {
    pub line: usize,
    pub col: u16,
}

/// One buffer line as text, with the column of each cell's first byte.
#[derive(Debug, Default, Clone)]
pub struct Line {
    pub text: String,
    /// (byte offset in `text`, column) per cell, in order.
    pub cells: Vec<(usize, u16)>,
    pub wrapped: bool,
}

impl Line {
    /// Visible row `row` of `screen` (at its current scroll offset).
    pub fn from_screen(screen: &vt100::Screen, row: u16) -> Line {
        let (_, cols) = screen.size();
        let mut l = Line { wrapped: screen.row_wrapped(row), ..Line::default() };
        for col in 0..cols {
            let cell = screen.cell(row, col);
            if cell.is_some_and(|c| c.is_wide_continuation()) {
                continue;
            }
            l.cells.push((l.text.len(), col));
            match cell.filter(|c| c.has_contents()) {
                Some(c) => l.text.push_str(c.contents()),
                None => l.text.push(' '),
            }
        }
        l
    }

    fn char_at(&self, idx: usize) -> char {
        self.cells.get(idx).and_then(|&(b, _)| self.text[b..].chars().next()).unwrap_or(' ')
    }

    /// Index of the cell covering `col`.
    fn idx(&self, col: u16) -> usize {
        self.cells.iter().rposition(|&(_, c)| c <= col).unwrap_or(0)
    }

    fn col_of_byte(&self, b: usize) -> u16 {
        self.cells.iter().rev().find(|&&(o, _)| o <= b).map_or(0, |&(_, c)| c)
    }

    /// Text of columns `from..=to`.
    fn slice(&self, from: u16, to: u16) -> &str {
        let start = self.cells.get(self.idx(from)).map_or(self.text.len(), |c| c.0);
        let end = self.cells.iter().find(|&&(_, c)| c > to).map_or(self.text.len(), |c| c.0);
        &self.text[start..end.max(start)]
    }

    fn last_nonblank(&self) -> u16 {
        (0..self.cells.len()).rev().find(|&i| !self.char_at(i).is_whitespace()).map_or(0, |i| self.cells[i].1)
    }

    fn first_nonblank(&self) -> u16 {
        (0..self.cells.len()).find(|&i| !self.char_at(i).is_whitespace()).map_or(0, |i| self.cells[i].1)
    }
}

/// Every line of the parser's buffer (history then screen). Restores the
/// scroll offset. Returns (history length, lines).
pub fn read_lines(p: &mut Parser) -> (usize, Vec<Line>) {
    let screen = p.screen_mut();
    let off = screen.scrollback();
    let hist = history_len(screen);
    let (rows, _) = screen.size();
    let mut lines = Vec::with_capacity(hist + usize::from(rows));
    while lines.len() < hist {
        screen.set_scrollback(hist - lines.len());
        let n = (hist - lines.len()).min(usize::from(rows));
        for r in 0..n as u16 {
            lines.push(Line::from_screen(screen, r));
        }
    }
    screen.set_scrollback(0);
    for r in 0..rows {
        lines.push(Line::from_screen(screen, r));
    }
    screen.set_scrollback(off);
    (hist, lines)
}

/// Byte ranges of `query` in `text`; lowercase query = case-insensitive.
pub fn find_all(text: &str, query: &str) -> Vec<(usize, usize)> {
    if query.is_empty() {
        return vec![];
    }
    let fold = !query.chars().any(char::is_uppercase);
    let (hay, needle) = if fold { (text.to_lowercase(), query.to_lowercase()) } else { (text.to_string(), query.to_string()) };
    // Lowercasing can change byte lengths; only trust it when it didn't.
    let hay = if hay.len() == text.len() { hay } else { text.to_string() };
    let mut v = Vec::new();
    let mut from = 0;
    while let Some(i) = hay[from..].find(&needle) {
        let s = from + i;
        v.push((s, s + needle.len()));
        from = s + needle.len().max(1);
        while from < hay.len() && !hay.is_char_boundary(from) {
            from += 1;
        }
        if from >= hay.len() {
            break;
        }
    }
    v
}

pub enum KeyResult {
    Stay,
    Exit,
    /// Copy this text, then leave copy mode.
    Yank(String),
    Flash(String),
}

#[derive(Debug)]
pub struct Copy {
    pub pane: u32,
    pub cursor: Pos,
    /// Selection anchor and whether the selection is line-wise.
    pub sel: Option<(Pos, bool)>,
    /// Search prompt being typed: (backward, bytes).
    pub prompt: Option<(bool, Vec<u8>)>,
    /// Last search: (query, backward).
    pub search: Option<(String, bool)>,
    /// Entered with the mouse: leave when scrolled back to the bottom.
    pub mouse: bool,
    /// History length at the last sync.
    pub hist: usize,
    /// Scroll offset asked for before the daemon's history arrived.
    want_off: usize,
    /// Keys typed before the daemon's history arrived (replayed then).
    pub queued: Vec<Vec<u8>>,
    lines: Option<Vec<Line>>,
}

impl Copy {
    pub fn new(pane: u32, p: &mut Parser, mouse: bool) -> Copy {
        let hist = history_len(p.screen_mut());
        let (r, c) = p.screen().cursor_position();
        let top = hist - p.screen().scrollback();
        let cursor = Pos { line: top + usize::from(r), col: c };
        Copy { pane, cursor, sel: None, prompt: None, search: None, mouse, hist, want_off: 0, queued: vec![], lines: None }
    }

    fn rows(p: &Parser) -> usize {
        usize::from(p.screen().size().0)
    }

    fn total(&self, p: &Parser) -> usize {
        self.hist + Self::rows(p)
    }

    /// First absolute line in view.
    pub fn top(&self, p: &Parser) -> usize {
        self.hist.saturating_sub(p.screen().scrollback())
    }

    fn lines(&mut self, p: &mut Parser) -> &[Line] {
        if self.lines.is_none() {
            let (hist, lines) = read_lines(p);
            self.hist = hist;
            self.lines = Some(lines);
        }
        self.lines.as_deref().unwrap_or_default()
    }

    fn line(&mut self, p: &mut Parser, n: usize) -> Line {
        self.lines(p).get(n).cloned().unwrap_or_default()
    }

    /// The pane got output: refresh the history length, drop cached text.
    pub fn sync(&mut self, p: &mut Parser) {
        self.hist = history_len(p.screen_mut());
        self.lines = None;
        let last = self.total(p) - 1;
        self.cursor.line = self.cursor.line.min(last);
        if let Some((a, _)) = &mut self.sel {
            a.line = a.line.min(last);
        }
    }

    /// The pane's parser was replaced by one holding the daemon's history
    /// (`old_hist` lines before, view `old_off` lines up).
    pub fn on_history(&mut self, p: &mut Parser, old_hist: usize, old_off: usize) {
        let hist = history_len(p.screen_mut());
        let delta = hist.saturating_sub(old_hist);
        self.cursor.line += delta;
        if let Some((a, _)) = &mut self.sel {
            a.line += delta;
        }
        p.screen_mut().set_scrollback(old_off.max(self.want_off));
        self.want_off = 0;
        self.sync(p);
        self.clamp_cursor_to_view(p);
    }

    /// Scroll the view by `delta` lines (positive = up, into history); the
    /// cursor stays inside the view.
    pub fn scroll(&mut self, p: &mut Parser, delta: isize) {
        let off = p.screen().scrollback();
        let want = off.saturating_add_signed(delta);
        p.screen_mut().set_scrollback(want);
        if p.screen().scrollback() < want {
            self.want_off = want; // history not here yet
        }
        self.clamp_cursor_to_view(p);
    }

    fn clamp_cursor_to_view(&mut self, p: &Parser) {
        let top = self.top(p);
        self.cursor.line = self.cursor.line.clamp(top, top + Self::rows(p) - 1);
    }

    /// Scroll so the cursor is visible.
    fn follow(&mut self, p: &mut Parser) {
        let top = self.top(p);
        let rows = Self::rows(p);
        let new_top = if self.cursor.line < top {
            self.cursor.line
        } else if self.cursor.line >= top + rows {
            self.cursor.line + 1 - rows
        } else {
            return;
        };
        p.screen_mut().set_scrollback(self.hist.saturating_sub(new_top));
    }

    pub fn at_bottom(&self, p: &Parser) -> bool {
        p.screen().scrollback() == 0
    }

    fn move_lines(&mut self, p: &mut Parser, delta: isize) {
        let last = self.total(p) - 1;
        self.cursor.line = self.cursor.line.saturating_add_signed(delta).min(last);
    }

    /// Move the cursor and the view together (Ctrl-u/d, PgUp/PgDn).
    fn page(&mut self, p: &mut Parser, delta: isize) {
        let row = self.cursor.line.saturating_sub(self.top(p));
        let off = p.screen().scrollback();
        p.screen_mut().set_scrollback(off.saturating_add_signed(-delta));
        let top = self.top(p);
        self.cursor.line = top + row;
        if p.screen().scrollback() == off {
            // View couldn't move (top/bottom): move the cursor instead.
            self.move_lines(p, delta);
        }
    }

    pub fn key(&mut self, p: &mut Parser, key: &[u8]) -> KeyResult {
        if let Some((back, mut q)) = self.prompt.take() {
            match key {
                b"\r" | b"\n" => {
                    let q = String::from_utf8_lossy(&q).into_owned();
                    if q.is_empty() {
                        return KeyResult::Stay;
                    }
                    self.search = Some((q, back));
                    return self.search_next(p, false);
                }
                b"\x1b" | b"\x03" => {}
                b"\x7f" | b"\x08" => {
                    while q.pop().is_some_and(|b| b & 0xc0 == 0x80) {}
                    self.prompt = Some((back, q));
                }
                k if k.first().is_some_and(|&b| b >= 0x20 && b != 0x7f) => {
                    q.extend_from_slice(k);
                    self.prompt = Some((back, q));
                }
                _ => self.prompt = Some((back, q)),
            }
            return KeyResult::Stay;
        }
        let rows = Self::rows(p) as isize;
        let cols = p.screen().size().1;
        match key {
            b"q" | b"\x1b" | b"\x03" => return KeyResult::Exit,
            b"h" | b"\x1b[D" | b"\x7f" | b"\x08" => self.cursor.col = self.cursor.col.saturating_sub(1),
            b"l" | b"\x1b[C" | b" " => self.cursor.col = (self.cursor.col + 1).min(cols - 1),
            b"j" | b"\x1b[B" => self.move_lines(p, 1),
            b"k" | b"\x1b[A" => self.move_lines(p, -1),
            b"0" | b"\x1b[H" | b"\x1b[1~" => self.cursor.col = 0,
            b"^" => self.cursor.col = self.line(p, self.cursor.line).first_nonblank(),
            b"$" | b"\x1b[F" | b"\x1b[4~" => self.cursor.col = self.line(p, self.cursor.line).last_nonblank(),
            b"w" => self.word(p, Word::NextStart),
            b"b" => self.word(p, Word::PrevStart),
            b"e" => self.word(p, Word::NextEnd),
            b"g" => self.cursor = Pos::default(),
            b"G" => {
                let line = self.total(p) - 1;
                self.cursor = Pos { line, col: 0 };
            }
            b"H" => self.cursor.line = self.top(p),
            b"M" => self.cursor.line = self.top(p) + (rows as usize) / 2,
            b"L" => self.cursor.line = self.top(p) + rows as usize - 1,
            b"\x15" => self.page(p, -(rows / 2).max(1)),
            b"\x04" => self.page(p, (rows / 2).max(1)),
            b"\x1b[5~" | b"\x02" => self.page(p, -rows),
            b"\x1b[6~" | b"\x06" => self.page(p, rows),
            b"/" => self.prompt = Some((false, vec![])),
            b"?" => self.prompt = Some((true, vec![])),
            b"n" => return self.search_next(p, false),
            b"N" => return self.search_next(p, true),
            b"v" => self.sel = if matches!(self.sel, Some((_, false))) { None } else { Some((self.cursor, false)) },
            b"V" => self.sel = if matches!(self.sel, Some((_, true))) { None } else { Some((self.cursor, true)) },
            b"y" | b"\r" | b"\n" => {
                return match self.selection_text(p) {
                    Some(t) => KeyResult::Yank(t),
                    None if key == b"y" => KeyResult::Flash("nothing selected (v to select)".into()),
                    None => KeyResult::Exit,
                };
            }
            _ => return KeyResult::Stay,
        }
        self.follow(p);
        KeyResult::Stay
    }

    /// Jump to the next match of the last search (`reverse` flips its direction).
    fn search_next(&mut self, p: &mut Parser, reverse: bool) -> KeyResult {
        let Some((q, back)) = self.search.clone() else { return KeyResult::Flash("no previous search".into()) };
        let back = back != reverse;
        let cur = self.cursor;
        let lines = self.lines(p);
        let n = lines.len();
        let hit = |l: usize| -> Vec<Pos> {
            find_all(&lines[l].text, &q).into_iter().map(|(s, _)| Pos { line: l, col: lines[l].col_of_byte(s) }).collect()
        };
        let mut found = None;
        // Current line first (after/before the cursor), then the others, wrapping.
        for k in 0..=n {
            let l = if back { (cur.line + n - k % n) % n } else { (cur.line + k) % n };
            let hits = hit(l);
            let pick = if back {
                hits.into_iter().rev().find(|h| k > 0 || h.col < cur.col)
            } else {
                hits.into_iter().find(|h| k > 0 || h.col > cur.col)
            };
            if pick.is_some() {
                found = pick;
                break;
            }
        }
        match found {
            Some(pos) => {
                self.cursor = pos;
                self.follow(p);
                KeyResult::Stay
            }
            None => KeyResult::Flash(format!("not found: {q}")),
        }
    }

    fn word(&mut self, p: &mut Parser, w: Word) {
        let cur = self.cursor;
        let lines = self.lines(p);
        let class = |l: usize, i: usize| {
            let c = lines[l].char_at(i);
            if c.is_whitespace() {
                0
            } else if c.is_alphanumeric() || c == '_' {
                1
            } else {
                2
            }
        };
        let next = |(l, i): (usize, usize)| {
            if i + 1 < lines[l].cells.len() {
                Some((l, i + 1))
            } else if l + 1 < lines.len() {
                Some((l + 1, 0))
            } else {
                None
            }
        };
        let prev = |(l, i): (usize, usize)| {
            if i > 0 {
                Some((l, i - 1))
            } else if l > 0 {
                Some((l - 1, lines[l - 1].cells.len().saturating_sub(1)))
            } else {
                None
            }
        };
        let Some(line) = lines.get(cur.line) else { return };
        let mut at = (cur.line, line.idx(cur.col));
        match w {
            Word::NextStart => {
                let c0 = class(at.0, at.1);
                while c0 != 0 && class(at.0, at.1) == c0 {
                    match next(at) {
                        Some(n) => at = n,
                        None => break,
                    }
                }
                while class(at.0, at.1) == 0 {
                    match next(at) {
                        Some(n) => at = n,
                        None => break,
                    }
                }
            }
            Word::NextEnd => {
                if let Some(n) = next(at) {
                    at = n;
                }
                while class(at.0, at.1) == 0 {
                    match next(at) {
                        Some(n) => at = n,
                        None => break,
                    }
                }
                let c = class(at.0, at.1);
                while let Some(n) = next(at).filter(|&(l, i)| class(l, i) == c) {
                    at = n;
                }
            }
            Word::PrevStart => {
                if let Some(n) = prev(at) {
                    at = n;
                }
                while class(at.0, at.1) == 0 {
                    match prev(at) {
                        Some(n) => at = n,
                        None => break,
                    }
                }
                let c = class(at.0, at.1);
                while let Some(n) = prev(at).filter(|&(l, i)| class(l, i) == c) {
                    at = n;
                }
            }
        }
        let col = lines[at.0].cells.get(at.1).map_or(0, |c| c.1);
        self.cursor = Pos { line: at.0, col };
    }

    /// Ordered selection bounds, or None.
    fn bounds(&self) -> Option<(Pos, Pos, bool)> {
        let (a, line) = self.sel?;
        let (s, e) = if a <= self.cursor { (a, self.cursor) } else { (self.cursor, a) };
        Some((s, e, line))
    }

    pub fn selected(&self, line: usize, col: u16) -> bool {
        match self.bounds() {
            Some((s, e, true)) => (s.line..=e.line).contains(&line),
            Some((s, e, false)) => {
                let p = Pos { line, col };
                s <= p && p <= e
            }
            None => false,
        }
    }

    /// Selected text: trailing blanks trimmed, wrapped lines joined.
    pub fn selection_text(&mut self, p: &mut Parser) -> Option<String> {
        let (s, e, linewise) = self.bounds()?;
        let lines = self.lines(p);
        let mut out = String::new();
        for (l, line) in lines.iter().enumerate().take(e.line + 1).skip(s.line) {
            let from = if linewise || l > s.line { 0 } else { s.col };
            let to = if linewise || l < e.line { u16::MAX } else { e.col };
            let last = l == e.line;
            let text = line.slice(from, to);
            if line.wrapped && !last && !linewise {
                out.push_str(text);
            } else {
                out.push_str(text.trim_end());
                if !last || linewise {
                    out.push('\n');
                }
            }
        }
        Some(out)
    }

    /// Draw selection, search matches, the position indicator and return the
    /// cursor cell, for the pane drawn at `area` from `screen` (whose scroll
    /// offset is this copy mode's view).
    pub fn overlay(&self, screen: &vt100::Screen, area: Rect, buf: &mut Buffer, accent: Color) -> Option<(u16, u16)> {
        let off = screen.scrollback();
        let top = self.hist.saturating_sub(off);
        let (rows, cols) = screen.size();
        let (rows, cols) = (rows.min(area.height), cols.min(area.width));
        let mark = Style::new().bg(accent).fg(Color::Black);
        for r in 0..rows {
            let line = top + usize::from(r);
            if let Some((q, _)) = &self.search {
                let l = Line::from_screen(screen, r);
                for (s, e) in find_all(&l.text, q) {
                    let (c0, c1) = (l.col_of_byte(s), l.col_of_byte(e.saturating_sub(1)));
                    for c in c0..=c1.min(cols - 1) {
                        buf[(area.x + c, area.y + r)].set_style(mark);
                    }
                }
            }
            if self.sel.is_some() {
                for c in 0..cols {
                    if self.selected(line, c) {
                        let cell = &mut buf[(area.x + c, area.y + r)];
                        cell.set_style(Style::new().add_modifier(Modifier::REVERSED));
                    }
                }
            }
        }
        let label = format!("[{off}/{}]", self.hist);
        let w = label.len() as u16;
        if w <= cols {
            buf.set_string(area.x + cols - w, area.y, &label, mark);
        }
        let row = self.cursor.line.checked_sub(top).filter(|&r| r < usize::from(rows))? as u16;
        (self.cursor.col < cols).then_some((area.x + self.cursor.col, area.y + row))
    }
}

#[derive(Clone, Copy)]
enum Word {
    NextStart,
    PrevStart,
    NextEnd,
}

// --- App glue.

impl App {
    /// Enter copy mode on `pane` (`mouse`: entered by wheel/drag).
    pub(super) fn enter_copy(&mut self, pane: u32, mouse: bool) {
        let Some(p) = self.screens.get_mut(&pane) else {
            self.input.mode = Mode::Normal;
            return;
        };
        self.copy = Some(Copy::new(pane, p, mouse));
        self.input.mode = Mode::Copy;
        if !self.have_history.contains(&pane) {
            self.outbox.push(ClientMsg::History(pane));
        }
    }

    pub(super) fn exit_copy(&mut self) {
        if let Some(c) = self.copy.take()
            && let Some(p) = self.screens.get_mut(&c.pane)
        {
            p.screen_mut().set_scrollback(0);
        }
        if matches!(self.input.mode, Mode::Copy) {
            self.input.mode = Mode::Normal;
        }
    }

    pub(super) fn copy_key(&mut self, key: &[u8]) {
        let Some(c) = self.copy.as_mut() else { return };
        if !self.have_history.contains(&c.pane) {
            c.queued.push(key.to_vec());
            return;
        }
        let Some(p) = self.screens.get_mut(&c.pane) else { return self.exit_copy() };
        match c.key(p, key) {
            KeyResult::Stay => {}
            KeyResult::Exit => self.exit_copy(),
            KeyResult::Flash(m) => self.flash = Some(m),
            KeyResult::Yank(text) => {
                self.flash = Some(clipboard::copy(text.as_bytes()));
                self.exit_copy();
            }
        }
    }

    /// Status-bar text for copy mode: the search prompt while typing.
    pub(super) fn copy_prompt(&self) -> Option<String> {
        let (back, q) = self.copy.as_ref()?.prompt.as_ref()?;
        Some(format!("{}{}", if *back { '?' } else { '/' }, String::from_utf8_lossy(q)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 5×20 pane: lines "line 1".."line 30", then "foo bar_baz qux", "$ ".
    fn fixture() -> Parser {
        let mut p = Parser::new(5, 20, 1000);
        for i in 1..=30 {
            p.process(format!("line {i}\r\n").as_bytes());
        }
        p.process(b"foo.bar_baz  qux\r\n$ ");
        p
    }

    fn keys(c: &mut Copy, p: &mut Parser, ks: &[&[u8]]) {
        for k in ks {
            c.key(p, k);
        }
    }

    #[test]
    fn starts_at_terminal_cursor() {
        let mut p = fixture();
        let c = Copy::new(1, &mut p, false);
        assert_eq!(c.hist, 27); // 32 lines, 5 on screen
        assert_eq!(c.cursor, Pos { line: 31, col: 2 });
    }

    #[test]
    fn motions_scroll_the_view() {
        let mut p = fixture();
        let mut c = Copy::new(1, &mut p, false);
        keys(&mut c, &mut p, &[b"g"]);
        assert_eq!(c.cursor, Pos { line: 0, col: 0 });
        assert_eq!(p.screen().scrollback(), 27);
        assert_eq!(p.screen().rows(0, 20).next().unwrap(), "line 1");
        keys(&mut c, &mut p, &[b"j", b"j", b"$"]);
        assert_eq!(c.cursor, Pos { line: 2, col: 5 });
        keys(&mut c, &mut p, &[b"0", b"l", b"\x1b[C"]);
        assert_eq!(c.cursor.col, 2);
        keys(&mut c, &mut p, &[b"G"]);
        assert_eq!(c.cursor.line, 31);
        assert_eq!(p.screen().scrollback(), 0);
        keys(&mut c, &mut p, &[b"\x15"]); // half page up
        assert_eq!(p.screen().scrollback(), 2);
        keys(&mut c, &mut p, &[b"\x1b[5~"]);
        assert_eq!(p.screen().scrollback(), 7);
        keys(&mut c, &mut p, &[b"\x04", b"\x1b[6~"]);
        assert_eq!(p.screen().scrollback(), 0);
    }

    #[test]
    fn word_motions() {
        let mut p = fixture();
        let mut c = Copy::new(1, &mut p, false);
        c.cursor = Pos { line: 30, col: 0 }; // "foo.bar_baz  qux"
        let mut cols = vec![];
        for _ in 0..4 {
            c.key(&mut p, b"w");
            cols.push((c.cursor.line, c.cursor.col));
        }
        assert_eq!(cols, [(30, 3), (30, 4), (30, 13), (31, 0)]);
        c.key(&mut p, b"b");
        assert_eq!((c.cursor.line, c.cursor.col), (30, 13));
        c.key(&mut p, b"b");
        assert_eq!(c.cursor.col, 4);
        c.key(&mut p, b"e");
        assert_eq!(c.cursor.col, 10);
    }

    #[test]
    fn search_forward_backward_wraps() {
        let mut p = fixture();
        let mut c = Copy::new(1, &mut p, false);
        keys(&mut c, &mut p, &[b"?", b"l", b"i", b"n", b"e", b" ", b"1", b"2", b"\r"]);
        assert_eq!(c.cursor, Pos { line: 11, col: 0 });
        assert!(p.screen().scrollback() > 0);
        assert!(c.cursor_visible(&p));
        c.key(&mut p, b"n"); // only one "line 12": wraps around to it
        assert_eq!(c.cursor.line, 11);
        // Uppercase query is case-sensitive: no match.
        keys(&mut c, &mut p, &[b"/", b"L", b"I", b"N", b"E"]);
        assert!(matches!(c.key(&mut p, b"\r"), KeyResult::Flash(_)));
        keys(&mut c, &mut p, &[b"/", b"q", b"u", b"x", b"\r"]);
        assert_eq!(c.cursor, Pos { line: 30, col: 13 });
        c.key(&mut p, b"N");
        assert_eq!(c.cursor, Pos { line: 30, col: 13 });
        // Backspace edits the prompt.
        keys(&mut c, &mut p, &[b"/", b"x", b"\x7f", b"$", b"\r"]);
        assert_eq!(c.cursor, Pos { line: 31, col: 0 });
    }

    #[test]
    fn selection_and_yank() {
        let mut p = fixture();
        let mut c = Copy::new(1, &mut p, false);
        c.cursor = Pos { line: 30, col: 4 };
        keys(&mut c, &mut p, &[b"v", b"e"]);
        assert!(c.selected(30, 4) && c.selected(30, 10) && !c.selected(30, 11));
        match c.key(&mut p, b"y") {
            KeyResult::Yank(t) => assert_eq!(t, "bar_baz"),
            _ => panic!("expected yank"),
        }
        // Line-wise over two lines.
        c.sel = None;
        c.cursor = Pos { line: 0, col: 3 };
        keys(&mut c, &mut p, &[b"V", b"j"]);
        assert_eq!(c.selection_text(&mut p).unwrap(), "line 1\nline 2\n");
        // Character-wise across lines, backwards.
        c.sel = None;
        c.cursor = Pos { line: 1, col: 2 };
        keys(&mut c, &mut p, &[b"v", b"k", b"l"]);
        assert_eq!(c.selection_text(&mut p).unwrap(), "e 1\nlin");
        assert!(matches!(c.key(&mut p, b"\x1b"), KeyResult::Exit));
    }

    #[test]
    fn wrapped_lines_join_when_copied() {
        let mut p = Parser::new(5, 10, 100);
        p.process(b"0123456789abcdef\r\n");
        let mut c = Copy::new(1, &mut p, false);
        c.cursor = Pos { line: 0, col: 0 };
        keys(&mut c, &mut p, &[b"v", b"j", b"$"]);
        assert_eq!(c.selection_text(&mut p).unwrap(), "0123456789abcdef");
    }

    #[test]
    fn view_stays_put_while_output_arrives() {
        let mut p = fixture();
        let mut c = Copy::new(1, &mut p, true);
        c.scroll(&mut p, 10);
        let before: Vec<String> = p.screen().rows(0, 20).collect();
        p.process(b"\r\nmore\r\nand more\r\n");
        c.sync(&mut p);
        assert_eq!(p.screen().rows(0, 20).collect::<Vec<_>>(), before);
        assert_eq!(c.hist, 30);
        c.scroll(&mut p, -100);
        assert!(c.at_bottom(&p));
    }

    #[test]
    fn find_all_smart_case() {
        assert_eq!(find_all("Foo foo FOO", "foo"), [(0, 3), (4, 7), (8, 11)]);
        assert_eq!(find_all("Foo foo FOO", "Foo"), [(0, 3)]);
        assert_eq!(find_all("aaa", "aa"), [(0, 2)]);
        assert!(find_all("x", "").is_empty());
    }

    impl Copy {
        fn cursor_visible(&self, p: &Parser) -> bool {
            let top = self.top(p);
            (top..top + Copy::rows(p)).contains(&self.cursor.line)
        }
    }
}
