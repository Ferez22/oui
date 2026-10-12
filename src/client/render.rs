//! Drawing: tab bar, the active tab's panes with borders, status bar, picker.
//! Colours come from the config theme; status bar segments from `[status]`.

use super::input::Mode;
use super::{App, picker_options};
use crate::config::{Action, Keys, Segment};
use crate::layout;
use crate::protocol::{Layout, TabInfo};
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout as Split, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, List, ListItem, ListState, Paragraph};

pub fn draw(app: &App, f: &mut Frame) {
    let [tabs_area, pane_area, status_area] =
        Split::vertical([Constraint::Length(1), Constraint::Min(1), Constraint::Length(1)]).areas(f.area());
    let Some(layout) = &app.layout else {
        f.render_widget(Paragraph::new(" connecting…"), pane_area);
        return;
    };
    let theme = &app.config.theme;
    let active = layout.tabs.get(layout.active);
    let pcolor = active.map(|t| color(&t.color)).unwrap_or(Color::Gray);

    // Tab bar: each tab shows its number and profile, in the profile's colour.
    let bar = Style::new().bg(theme.tab_bar_bg.to_color(pcolor)).fg(theme.tab_bar_fg.to_color(pcolor));
    let mut spans = vec![Span::styled(TAB_BAR_LEAD, Style::new().add_modifier(Modifier::BOLD))];
    for (i, t) in layout.tabs.iter().enumerate() {
        let c = color(&t.color);
        let style = if i == layout.active {
            Style::new()
                .bg(theme.tab_active_bg.to_color(c))
                .fg(theme.tab_active_fg.to_color(c))
                .add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(c)
        };
        spans.push(Span::styled(tab_label(i, t), style));
        spans.push(Span::raw(TAB_GAP));
    }
    f.render_widget(Paragraph::new(Line::from(spans)).style(bar), tabs_area);

    // Right panel: takes its columns from the pane area (the daemon was told).
    let panel_w = app.panel_width().min(pane_area.width);
    let [pane_area, panel_area] =
        Split::horizontal([Constraint::Min(1), Constraint::Length(panel_w)]).areas(pane_area);
    let ostyle = super::picker::OverlayStyle::new(theme, pcolor);
    if panel_w > 0 {
        let rows = app.panel.rows(&app.config.panel.widgets, &app.here(), theme.accent.to_color(pcolor));
        let footer = if app.panel.focused {
            "j/k move · ⏎ open · esc back".to_string()
        } else {
            let k = app.input.keys.key_for(Action::FocusPanel).map(display_key);
            k.map(|k| format!("{} {k} focus", display_key(&app.input.keys.prefix.notation))).unwrap_or_default()
        };
        app.panel.draw(f, panel_area, rows, &ostyle, &footer);
    }

    if let Some(tab) = active {
        draw_tab(app, tab, pane_area, f);
    }

    // Status bar: left segments, then right segments flush right. A flash
    // message takes the place of the hints (or leads the right side).
    let ctx = StatusCtx { app, active, pcolor };
    let left = ctx.line(&app.config.status.left, None);
    let flash = app.flash.as_deref().map(|m| m.replace('\n', " · "));
    let right = ctx.line(&app.config.status.right, flash.as_deref());
    let [l, r] = Split::horizontal([Constraint::Min(1), Constraint::Length(right.width() as u16)]).areas(status_area);
    let bar = Style::new().bg(theme.status_bg.to_color(pcolor)).fg(theme.status_fg.to_color(pcolor));
    f.render_widget(Paragraph::new(left).style(bar), l);
    f.render_widget(Paragraph::new(right).style(bar), r);

    if let Mode::Picker(sel) = app.input.mode {
        draw_picker(f, sel, &ostyle);
    }
    let cursor = match &app.overlay {
        Some(super::Overlay::Sessions(p)) => {
            super::picker::draw_sessions(f, p, &app.panel.sessions, &layout.session, &ostyle)
        }
        Some(super::Overlay::Rename(p)) => Some(super::picker::draw_prompt(f, p, &ostyle, "⏎ save · empty clears · esc cancel")),
        _ => None,
    };
    if let Some(pos) = cursor {
        f.set_cursor_position(pos);
    }
}

/// Tab bar label: " 1 profile " or " 1 name · profile " (the profile always
/// shows). Also used for mouse hit-testing (`tab_at`).
pub fn tab_label(i: usize, t: &TabInfo) -> String {
    match &t.name {
        Some(name) => format!(" {} {name} · {} ", i + 1, t.profile),
        None => format!(" {} {} ", i + 1, t.profile),
    }
}

const TAB_BAR_LEAD: &str = " oUI ";
const TAB_GAP: &str = " ";

/// The tab whose label covers column `x` of the tab bar.
pub fn tab_at(layout: &Layout, x: u16) -> Option<usize> {
    let mut start = Line::raw(TAB_BAR_LEAD).width();
    for (i, t) in layout.tabs.iter().enumerate() {
        let w = Line::raw(tab_label(i, t)).width();
        if (start..start + w).contains(&usize::from(x)) {
            return Some(i);
        }
        start += w + TAB_GAP.len();
    }
    None
}

struct StatusCtx<'a> {
    app: &'a App,
    active: Option<&'a TabInfo>,
    pcolor: Color,
}

impl StatusCtx<'_> {
    /// Segments joined by "│"; the mode badge needs no separator.
    fn line(&self, segments: &[Segment], flash: Option<&str>) -> Line<'static> {
        let mut spans: Vec<Span<'static>> = Vec::new();
        let mut flash = flash;
        let mut need_sep = false;
        let mut push = |spans: &mut Vec<Span<'static>>, seg: Vec<Span<'static>>, badge: bool| {
            if seg.is_empty() {
                return;
            }
            if need_sep && !badge {
                spans.push(Span::raw("│"));
            }
            need_sep = !badge;
            spans.extend(seg);
        };
        if let Some(m) = flash.filter(|_| !segments.contains(&Segment::Hints)) {
            push(&mut spans, vec![Span::raw(format!(" {m} "))], false);
            flash = None;
        }
        for seg in segments {
            let badge = *seg == Segment::Mode;
            let s = match (seg, flash.take_if(|_| *seg == Segment::Hints)) {
                (_, Some(m)) => vec![Span::raw(format!(" {m} "))],
                _ => self.segment(seg),
            };
            push(&mut spans, s, badge);
        }
        Line::from(spans)
    }

    fn segment(&self, seg: &Segment) -> Vec<Span<'static>> {
        let app = self.app;
        let theme = &app.config.theme;
        let badge = |text: &str, bg: Color| {
            Span::styled(format!(" {text} "), Style::new().bg(bg).fg(theme.tab_active_fg.to_color(self.pcolor)))
        };
        match seg {
            Segment::Mode => {
                let mut v = Vec::new();
                let mode = match (&app.input.mode, &app.overlay) {
                    (Mode::Normal, _) => None,
                    (Mode::Copy, _) => Some("COPY"),
                    (Mode::Overlay, Some(super::Overlay::Panel)) => Some("PANEL"),
                    (Mode::Overlay, Some(super::Overlay::Sessions(_))) => Some("SESSIONS"),
                    (Mode::Overlay, Some(super::Overlay::Rename(_))) => Some("RENAME"),
                    _ => Some("PREFIX"),
                };
                if let Some(m) = mode {
                    v.push(badge(m, theme.accent.to_color(self.pcolor)));
                }
                if matches!(app.input.mode, Mode::Copy)
                    && let Some(p) = app.copy_prompt()
                {
                    v.push(Span::raw(format!(" {p}▏")));
                }
                if self.active.is_some_and(|t| t.zoomed) {
                    v.push(badge("ZOOM", self.pcolor));
                }
                v
            }
            Segment::Session => {
                let name = app.layout.as_ref().map(|l| l.session.clone()).unwrap_or_default();
                vec![Span::styled(format!(" {name} "), Style::new().add_modifier(Modifier::BOLD))]
            }
            Segment::Profile => {
                let name = self.active.map(|t| t.profile.as_str()).unwrap_or("-");
                vec![Span::styled(format!(" ● {name} "), Style::new().fg(self.pcolor))]
            }
            Segment::Hints => vec![Span::raw(format!("{} ", hints(&app.input.keys)))],
            Segment::Clock => vec![Span::raw(format!(" {} ", clock()))],
            Segment::Hostname => vec![Span::raw(format!(" {} ", app.hostname))],
            Segment::TabsCount => {
                let n = app.layout.as_ref().map_or(0, |l| l.tabs.len());
                vec![Span::raw(format!(" {n} tab{} ", if n == 1 { "" } else { "s" }))]
            }
            Segment::Text(t) => vec![Span::raw(t.clone())],
            // The client doesn't know the focused shell's directory.
            Segment::Cwd => vec![],
        }
    }
}

/// Key hints from the actual bindings, e.g.
/// `^b c tab · %/" split · ←↓↑→ focus · z zoom · x close · d detach`.
pub fn hints(keys: &Keys) -> String {
    let key = |a: Action| keys.key_for(a).map(display_key);
    let mut parts = vec![display_key(&keys.prefix.notation)];
    let mut add = |keys: Vec<Option<String>>, label: &str, sep: &str| {
        let keys: Vec<String> = keys.into_iter().flatten().collect();
        if !keys.is_empty() {
            parts.push(format!("{} {label}", keys.join(sep)));
        }
    };
    add(vec![key(Action::NewTab)], "tab", "");
    add(vec![key(Action::SplitRight), key(Action::SplitDown)], "split", "/");
    let focus = vec![key(Action::FocusLeft), key(Action::FocusDown), key(Action::FocusUp), key(Action::FocusRight)];
    let all_arrows = focus.iter().all(|k| k.as_ref().is_some_and(|k| "←↓↑→".contains(k.as_str())));
    add(focus, "focus", if all_arrows { "" } else { "/" });
    add(vec![key(Action::Zoom)], "zoom", "");
    add(vec![key(Action::ClosePane)], "close", "");
    add(vec![key(Action::Detach)], "detach", "");
    // "^b c tab · …": the prefix leads the first hint without a separator.
    let first = parts.remove(0);
    match parts.split_first() {
        Some((h, rest)) => std::iter::once(format!("{first} {h}")).chain(rest.iter().cloned()).collect::<Vec<_>>().join(" · "),
        None => first,
    }
}

/// Short form of a key for hints: "C-b" → "^b", "Left" → "←".
pub fn display_key(notation: &str) -> String {
    match notation {
        "Left" => "←".into(),
        "Down" => "↓".into(),
        "Up" => "↑".into(),
        "Right" => "→".into(),
        n => match n.strip_prefix("C-") {
            Some(rest) if !rest.is_empty() => format!("^{rest}"),
            _ => n.into(),
        },
    }
}

/// Local time as HH:MM.
fn clock() -> String {
    // SAFETY: time/localtime_r write only into the provided, zeroed struct.
    unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&t, &mut tm).is_null() {
            return String::new();
        }
        format!("{:02}:{:02}", tm.tm_hour, tm.tm_min)
    }
}

/// Short host name (up to the first dot).
pub fn hostname() -> String {
    let mut buf = [0u8; 256];
    // SAFETY: gethostname writes at most buf.len() bytes into buf.
    if unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } != 0 {
        return String::new();
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    let name = String::from_utf8_lossy(&buf[..end]);
    name.split('.').next().unwrap_or_default().to_string()
}

/// Draw the tab's panes at their rects, borders between them (the focused
/// pane's edges in the profile colour), and the focused pane's cursor.
fn draw_tab(app: &App, tab: &TabInfo, area: Rect, f: &mut Frame) {
    let visible: Vec<_> = tab.panes.iter().filter(|p| !tab.zoomed || p.id == tab.focused).collect();
    let to_screen = |r: layout::Rect| {
        Rect::new(area.x + r.x, area.y + r.y, r.cols, r.rows).intersection(area)
    };
    let mut copy_cursor = None;
    for p in &visible {
        if let Some(parser) = app.screens.get(&p.id) {
            render_screen(parser.screen(), to_screen(p.rect), f.buffer_mut());
            if let Some(c) = app.copy.as_ref().filter(|c| c.pane == p.id) {
                let accent = app.config.theme.accent.to_color(color(&tab.color));
                copy_cursor = c.overlay(parser.screen(), to_screen(p.rect), f.buffer_mut(), accent);
            }
        }
    }
    if visible.len() > 1 {
        let focused = tab.panes.iter().find(|p| p.id == tab.focused).map(|p| to_screen(p.rect));
        let (theme, c) = (&app.config.theme, color(&tab.color));
        let colors = (theme.border.to_color(c), theme.focused_border.to_color(c));
        draw_borders(&visible.iter().map(|p| to_screen(p.rect)).collect::<Vec<_>>(), focused, colors, area, f.buffer_mut());
    }

    if let (Some(pos), Mode::Copy) = (copy_cursor, &app.input.mode) {
        f.set_cursor_position(pos);
        return;
    }
    let focused = visible.iter().find(|p| p.id == tab.focused);
    if let (Some(p), Mode::Normal) = (focused, &app.input.mode) {
        let Some(screen) = app.screens.get(&p.id).map(|s| s.screen()) else { return };
        let (r, c) = screen.cursor_position();
        if !screen.hide_cursor() && r < p.rect.rows && c < p.rect.cols {
            let rect = to_screen(p.rect);
            if r < rect.height && c < rect.width {
                f.set_cursor_position((rect.x + c, rect.y + r));
            }
        }
    }
}

/// Every cell of `area` not covered by a pane is a border; pick the box char
/// from which neighbouring cells are borders too.
/// `colors` is (border, focused border).
fn draw_borders(panes: &[Rect], focused: Option<Rect>, colors: (Color, Color), area: Rect, buf: &mut Buffer) {
    let inside = |r: &Rect, x: i32, y: i32| {
        x >= r.x as i32 && y >= r.y as i32 && x < (r.x + r.width) as i32 && y < (r.y + r.height) as i32
    };
    let is_border = |x: i32, y: i32| inside(&area, x, y) && !panes.iter().any(|p| inside(p, x, y));
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            let (xi, yi) = (x as i32, y as i32);
            if !is_border(xi, yi) {
                continue;
            }
            let (up, down, left, right) =
                (is_border(xi, yi - 1), is_border(xi, yi + 1), is_border(xi - 1, yi), is_border(xi + 1, yi));
            let sym = match (up, down, left, right) {
                (true, true, true, true) => "┼",
                (true, true, true, false) => "┤",
                (true, true, false, true) => "├",
                (false, true, true, true) => "┬",
                (true, false, true, true) => "┴",
                (false, true, false, true) => "┌",
                (false, true, true, false) => "┐",
                (true, false, false, true) => "└",
                (true, false, true, false) => "┘",
                (_, _, true, _) | (_, _, _, true) => "─",
                _ => "│",
            };
            let near_focus = focused.is_some_and(|r| (-1..=1).any(|dy| (-1..=1).any(|dx| inside(&r, xi + dx, yi + dy))));
            let fg = if near_focus { colors.1 } else { colors.0 };
            buf[(x, y)].set_symbol(sym).set_style(Style::new().fg(fg));
        }
    }
}

fn draw_picker(f: &mut Frame, sel: usize, st: &super::picker::OverlayStyle) {
    let opts = picker_options();
    let rect = super::picker::centered(f.area(), 36, opts.len() as u16 + 2);
    let items: Vec<ListItem> = opts
        .iter()
        .enumerate()
        .map(|(i, (name, c))| {
            ListItem::new(Line::from(vec![
                Span::raw(format!(" {i} ")),
                Span::styled("● ", Style::new().fg(color(c))),
                Span::raw(name.clone()),
            ]))
        })
        .collect();
    let list = List::new(items)
        .block(st.block(" new tab · profile "))
        .highlight_style(st.selected);
    let mut state = ListState::default().with_selected(Some(sel));
    f.render_widget(Clear, rect);
    f.render_stateful_widget(list, rect, &mut state);
}

pub fn color(name: &str) -> Color {
    name.parse().unwrap_or(Color::Blue)
}

fn vt_color(c: vt100::Color) -> Color {
    match c {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(i) => Color::Indexed(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

fn render_screen(screen: &vt100::Screen, area: Rect, buf: &mut Buffer) {
    let (rows, cols) = screen.size();
    for row in 0..rows.min(area.height) {
        for col in 0..cols.min(area.width) {
            let Some(cell) = screen.cell(row, col) else { continue };
            if cell.is_wide_continuation() {
                continue;
            }
            let mut style = Style::new().fg(vt_color(cell.fgcolor())).bg(vt_color(cell.bgcolor()));
            for (on, m) in [
                (cell.bold(), Modifier::BOLD),
                (cell.dim(), Modifier::DIM),
                (cell.italic(), Modifier::ITALIC),
                (cell.underline(), Modifier::UNDERLINED),
                (cell.inverse(), Modifier::REVERSED),
            ] {
                if on {
                    style = style.add_modifier(m);
                }
            }
            let sym = if cell.has_contents() { cell.contents() } else { " " };
            buf[(area.x + col, area.y + row)].set_symbol(sym).set_style(style);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn tab_label_keeps_profile() {
        let mut t = TabInfo { name: None, profile: "work".into(), color: "green".into(), panes: vec![], focused: 0, zoomed: false };
        assert_eq!(tab_label(0, &t), " 1 work ");
        t.name = Some("api".into());
        assert_eq!(tab_label(2, &t), " 3 api · work ");
    }

    #[test]
    fn tab_at_uses_named_labels() {
        let tab = |name: Option<&str>| TabInfo {
            name: name.map(Into::into),
            profile: "work".into(),
            color: "green".into(),
            panes: vec![],
            focused: 0,
            zoomed: false,
        };
        let layout = Layout { session: "s".into(), tabs: vec![tab(Some("api")), tab(None)], active: 0 };
        // " oUI " (0-4), " 1 api · work " (5-18), gap (19), " 2 work " (20-27).
        assert_eq!(tab_at(&layout, 4), None);
        assert_eq!(tab_at(&layout, 5), Some(0));
        assert_eq!(tab_at(&layout, 18), Some(0));
        assert_eq!(tab_at(&layout, 19), None);
        assert_eq!(tab_at(&layout, 20), Some(1));
        assert_eq!(tab_at(&layout, 27), Some(1));
        assert_eq!(tab_at(&layout, 28), None);
    }

    #[test]
    fn hints_follow_bindings() {
        assert_eq!(hints(&Keys::default()), "^b c tab · %/\" split · ←↓↑→ focus · z zoom · x close · d detach");
        let c = Config::parse(
            "[keys]\nprefix = \"C-a\"\n[keys.bindings]\nv = \"split-right\"\n\"%\" = \"none\"\n\"|\" = \"none\"\nUp = \"none\"\nz = \"none\"",
            None,
        )
        .unwrap();
        assert_eq!(hints(&c.keys), "^a c tab · v/\" split · ←/↓/k/→ focus · x close · d detach");
    }
}
