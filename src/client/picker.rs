//! Client overlays that take the keyboard: the session picker (`prefix s`) and
//! one-line prompts (rename tab, new session name). Pure key state machines
//! plus their drawing; the app turns outcomes into daemon messages.

use crate::config::Theme;
use crate::protocol::SessionInfo;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

const UP: &[u8] = b"\x1b[A";
const DOWN: &[u8] = b"\x1b[B";
const ESC: &[u8] = b"\x1b";
const CTRL_C: &[u8] = b"\x03";
const BACKSPACE: &[&[u8]] = &[b"\x7f", b"\x08"];
/// Ctrl-u: clear the line.
const KILL_LINE: &[u8] = b"\x15";

fn is_enter(key: &[u8]) -> bool {
    key == b"\r" || key == b"\n"
}

/// A one-line text prompt.
#[derive(Debug, Clone, PartialEq)]
pub struct Prompt {
    pub title: String,
    /// Raw bytes typed so far (UTF-8 arrives one byte per key).
    buf: Vec<u8>,
}

#[derive(Debug, PartialEq)]
pub enum PromptOutcome {
    Pending,
    Cancel,
    Submit(String),
}

impl Prompt {
    pub fn new(title: impl Into<String>, initial: &str) -> Self {
        Prompt { title: title.into(), buf: initial.as_bytes().to_vec() }
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.buf).into_owned()
    }

    pub fn key(&mut self, key: &[u8]) -> PromptOutcome {
        match key {
            k if is_enter(k) => return PromptOutcome::Submit(self.text().trim().to_string()),
            ESC | CTRL_C => return PromptOutcome::Cancel,
            k if BACKSPACE.contains(&k) => {
                // Drop one whole character, not one byte.
                let text = self.text();
                if let Some((i, _)) = text.char_indices().next_back() {
                    self.buf = text.as_bytes()[..i].to_vec();
                }
            }
            KILL_LINE => self.buf.clear(),
            [b] if *b >= 0x20 => self.buf.push(*b),
            _ => {} // other control keys and escape sequences
        }
        PromptOutcome::Pending
    }
}

/// Session picker: list → (n) name prompt, (x) kill confirmation.
#[derive(Debug, Clone, PartialEq)]
pub enum SessionPicker {
    List { sel: usize },
    NewName(Prompt),
    ConfirmKill { sel: usize, name: String },
}

#[derive(Debug, PartialEq)]
pub enum PickOutcome {
    Stay,
    Close,
    Switch(String),
    Create(String),
    Kill(String),
}

impl SessionPicker {
    /// Start on the current session.
    pub fn new(sessions: &[SessionInfo], current: &str) -> Self {
        SessionPicker::List { sel: sessions.iter().position(|s| s.name == current).unwrap_or(0) }
    }

    pub fn key(&mut self, key: &[u8], sessions: &[SessionInfo]) -> PickOutcome {
        let n = sessions.len();
        match self {
            SessionPicker::List { sel } => {
                if n > 0 {
                    *sel = (*sel).min(n - 1);
                }
                match key {
                    UP | b"k" if n > 0 => *sel = (*sel + n - 1) % n,
                    DOWN | b"j" if n > 0 => *sel = (*sel + 1) % n,
                    ESC | b"q" | CTRL_C => return PickOutcome::Close,
                    k if is_enter(k) => {
                        return sessions.get(*sel).map_or(PickOutcome::Stay, |s| PickOutcome::Switch(s.name.clone()));
                    }
                    b"n" => *self = SessionPicker::NewName(Prompt::new("new session", "")),
                    b"x" => {
                        if let Some(s) = sessions.get(*sel) {
                            *self = SessionPicker::ConfirmKill { sel: *sel, name: s.name.clone() };
                        }
                    }
                    _ => {}
                }
                PickOutcome::Stay
            }
            SessionPicker::NewName(p) => match p.key(key) {
                PromptOutcome::Pending => PickOutcome::Stay,
                PromptOutcome::Cancel => {
                    *self = SessionPicker::List { sel: 0 };
                    PickOutcome::Stay
                }
                PromptOutcome::Submit(name) if name.is_empty() => PickOutcome::Stay,
                PromptOutcome::Submit(name) => {
                    if sessions.iter().any(|s| s.name == name) {
                        PickOutcome::Switch(name)
                    } else {
                        PickOutcome::Create(name)
                    }
                }
            },
            SessionPicker::ConfirmKill { sel, name } => {
                let (sel, name) = (*sel, name.clone());
                if key == b"y" || key == b"Y" {
                    // The list loses a row: keep the selection on it.
                    *self = SessionPicker::List { sel: sel.min(n.saturating_sub(2)) };
                    PickOutcome::Kill(name)
                } else {
                    *self = SessionPicker::List { sel };
                    PickOutcome::Stay
                }
            }
        }
    }
}

/// Overlay colours from the theme's panel colours (new-tab picker, session
/// picker, prompts and the right panel all share them).
pub struct OverlayStyle {
    pub base: Style,
    pub border: Style,
    pub selected: Style,
    pub dim: Style,
}

impl OverlayStyle {
    pub fn new(theme: &Theme, profile: Color) -> Self {
        let bg = theme.panel_bg.to_color(profile);
        let fg = theme.panel_fg.to_color(profile);
        let accent = theme.accent.to_color(profile);
        OverlayStyle {
            base: Style::new().bg(bg).fg(fg),
            border: Style::new().bg(bg).fg(accent),
            selected: Style::new().bg(accent).fg(bg).add_modifier(Modifier::BOLD),
            dim: Style::new().bg(bg).fg(fg).add_modifier(Modifier::DIM),
        }
    }

    pub fn block<'a>(&self, title: impl Into<Line<'a>>) -> Block<'a> {
        Block::new().borders(Borders::ALL).border_style(self.border).style(self.base).title(title)
    }
}

/// A rect of `width`×`height` centred in `area` (clipped to it).
pub fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let (w, h) = (width.min(area.width), height.min(area.height));
    Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h }
}

/// Draw a prompt box; returns where the cursor goes.
pub fn draw_prompt(f: &mut Frame, p: &Prompt, st: &OverlayStyle, hint: &str) -> (u16, u16) {
    let rect = centered(f.area(), 44, 4);
    f.render_widget(Clear, rect);
    let text = p.text();
    let lines = vec![Line::from(format!(" {text}")), Line::styled(format!(" {hint}"), st.dim)];
    f.render_widget(Paragraph::new(lines).block(st.block(format!(" {} ", p.title))), rect);
    let w = Line::from(text).width() as u16;
    ((rect.x + 2 + w).min(rect.right().saturating_sub(2)), rect.y + 1)
}

/// Lines of the session list (also used by tests).
pub fn session_lines(sessions: &[SessionInfo], current: &str) -> Vec<String> {
    let width = sessions.iter().map(|s| s.name.chars().count()).max().unwrap_or(0);
    sessions
        .iter()
        .map(|s| {
            let mark = if s.name == current { "▸" } else { " " };
            let tabs = format!("{} tab{}", s.tabs, if s.tabs == 1 { "" } else { "s" });
            let attached = if s.name != current && s.clients > 0 { " · attached" } else { "" };
            format!("{mark} {:<width$}  {tabs}{attached}", s.name)
        })
        .collect()
}

/// Draw the session picker; returns the cursor position for a name prompt.
pub fn draw_sessions(
    f: &mut Frame,
    picker: &SessionPicker,
    sessions: &[SessionInfo],
    current: &str,
    st: &OverlayStyle,
) -> Option<(u16, u16)> {
    if let SessionPicker::NewName(p) = picker {
        return Some(draw_prompt(f, p, st, "⏎ create · esc back"));
    }
    let lines = session_lines(sessions, current);
    let (sel, footer) = match picker {
        SessionPicker::List { sel } => (*sel, "⏎ switch · n new · x kill · esc close".to_string()),
        SessionPicker::ConfirmKill { sel, name } => (*sel, format!("kill '{name}' and its shells? y/n")),
        SessionPicker::NewName(_) => unreachable!(),
    };
    let inner_w = lines.iter().map(|l| Line::from(l.as_str()).width()).chain([footer.chars().count() + 1, 30]).max().unwrap_or(30);
    let rect = centered(f.area(), inner_w as u16 + 4, lines.len().max(1) as u16 + 4);
    let mut out: Vec<Line> = if lines.is_empty() {
        vec![Line::styled(" no sessions", st.dim)]
    } else {
        lines
            .into_iter()
            .enumerate()
            .map(|(i, l)| {
                let style = if i == sel { st.selected } else { st.base };
                Line::from(Span::styled(format!(" {l:<w$}", w = inner_w.saturating_sub(1)), style))
            })
            .collect()
    };
    out.push(Line::raw(""));
    let footer_style = if matches!(picker, SessionPicker::ConfirmKill { .. }) { st.border.add_modifier(Modifier::BOLD) } else { st.dim };
    out.push(Line::styled(format!(" {footer}"), footer_style));
    f.render_widget(Clear, rect);
    f.render_widget(Paragraph::new(out).block(st.block(" sessions ")), rect);
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sessions(names: &[&str]) -> Vec<SessionInfo> {
        names.iter().enumerate().map(|(i, n)| SessionInfo { name: n.to_string(), tabs: i + 1, clients: 0 }).collect()
    }

    fn feed(p: &mut Prompt, s: &[u8]) -> PromptOutcome {
        let mut last = PromptOutcome::Pending;
        for b in s {
            last = p.key(&[*b]);
        }
        last
    }

    #[test]
    fn prompt_editing() {
        let mut p = Prompt::new("rename tab", "api");
        assert_eq!(p.key(b"\x7f"), PromptOutcome::Pending);
        assert_eq!(feed(&mut p, b"p-x"), PromptOutcome::Pending);
        assert_eq!(p.text(), "app-x");
        assert_eq!(p.key(b"\x1b[A"), PromptOutcome::Pending); // arrows ignored
        assert_eq!(feed(&mut p, "é\r".as_bytes()), PromptOutcome::Submit("app-xé".into()));
        p.key(b"\x7f");
        assert_eq!(p.text(), "app-x", "backspace removes a whole character");
        p.key(b"\x15");
        assert_eq!(feed(&mut p, b"  \r"), PromptOutcome::Submit(String::new()));
        assert_eq!(p.key(b"\x1b"), PromptOutcome::Cancel);
    }

    #[test]
    fn picker_navigation_and_switch() {
        let s = sessions(&["a", "b", "c"]);
        let mut p = SessionPicker::new(&s, "b");
        assert_eq!(p, SessionPicker::List { sel: 1 });
        assert_eq!(p.key(b"j", &s), PickOutcome::Stay);
        assert_eq!(p.key(b"j", &s), PickOutcome::Stay); // wraps
        assert_eq!(p, SessionPicker::List { sel: 0 });
        assert_eq!(p.key(b"\x1b[A", &s), PickOutcome::Stay);
        assert_eq!(p.key(b"\r", &s), PickOutcome::Switch("c".into()));
        assert_eq!(p.key(b"q", &s), PickOutcome::Close);
        assert_eq!(SessionPicker::new(&[], "x").key(b"\r", &[]), PickOutcome::Stay);
    }

    #[test]
    fn picker_new_session() {
        let s = sessions(&["a"]);
        let mut p = SessionPicker::new(&s, "a");
        p.key(b"n", &s);
        assert!(matches!(p, SessionPicker::NewName(_)));
        assert_eq!(p.key(b"\r", &s), PickOutcome::Stay, "empty name is ignored");
        for b in b"work" {
            p.key(&[*b], &s);
        }
        assert_eq!(p.key(b"\r", &s), PickOutcome::Create("work".into()));
        let mut p = SessionPicker::NewName(Prompt::new("new session", "a"));
        assert_eq!(p.key(b"\r", &s), PickOutcome::Switch("a".into()), "existing name switches");
        p.key(b"\x1b", &s);
        assert_eq!(p, SessionPicker::List { sel: 0 });
    }

    #[test]
    fn picker_kill_confirms() {
        let s = sessions(&["a", "b"]);
        let mut p = SessionPicker::List { sel: 1 };
        p.key(b"x", &s);
        assert_eq!(p, SessionPicker::ConfirmKill { sel: 1, name: "b".into() });
        assert_eq!(p.key(b"n", &s), PickOutcome::Stay);
        assert_eq!(p, SessionPicker::List { sel: 1 });
        p.key(b"x", &s);
        assert_eq!(p.key(b"y", &s), PickOutcome::Kill("b".into()));
        assert_eq!(p, SessionPicker::List { sel: 0 });
    }

    #[test]
    fn session_list_marks_current() {
        let mut s = sessions(&["main", "work"]);
        s[1].clients = 1;
        assert_eq!(session_lines(&s, "main"), ["▸ main  1 tab", "  work  2 tabs · attached"]);
    }
}
