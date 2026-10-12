//! Drawing: tab bar, the active tab's panes with borders, status bar, picker.

use super::input::Mode;
use super::{App, picker_options};
use crate::layout;
use crate::protocol::TabInfo;
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout as Split, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};

const BORDER: Color = Color::Indexed(240);

pub fn draw(app: &App, f: &mut Frame) {
    let [tabs_area, pane_area, status_area] =
        Split::vertical([Constraint::Length(1), Constraint::Min(1), Constraint::Length(1)]).areas(f.area());
    let Some(layout) = &app.layout else {
        f.render_widget(Paragraph::new(" connecting…"), pane_area);
        return;
    };
    let active = layout.tabs.get(layout.active);

    // Tab bar: each tab shows its number and profile, in the profile's colour.
    let mut spans = vec![Span::styled(" oUI ", Style::new().add_modifier(Modifier::BOLD))];
    for (i, t) in layout.tabs.iter().enumerate() {
        let c = color(&t.color);
        let style = if i == layout.active {
            Style::new().bg(c).fg(Color::Black).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(c)
        };
        spans.push(Span::styled(format!(" {} {} ", i + 1, t.profile), style));
        spans.push(Span::raw(" "));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), tabs_area);

    if let Some(tab) = active {
        draw_tab(app, tab, pane_area, f);
    }

    // Status bar.
    let (pname, pcolor) = active.map(|t| (t.profile.as_str(), color(&t.color))).unwrap_or(("-", Color::Gray));
    let mode = match app.input.mode {
        Mode::Normal => Span::raw(""),
        _ => Span::styled(" PREFIX ", Style::new().bg(Color::Yellow).fg(Color::Black)),
    };
    let zoom = match active {
        Some(t) if t.zoomed => Span::styled(" ZOOM ", Style::new().bg(pcolor).fg(Color::Black)),
        _ => Span::raw(""),
    };
    let left = Line::from(vec![
        mode,
        zoom,
        Span::styled(format!(" {} ", layout.session), Style::new().add_modifier(Modifier::BOLD)),
        Span::raw("│"),
        Span::styled(format!(" ● {pname} "), Style::new().fg(pcolor)),
    ]);
    let hint = app
        .flash
        .clone()
        .unwrap_or_else(|| "^b c tab · %/\" split · ←↓↑→ focus · z zoom · x close · d detach ".into());
    let [l, r] =
        Split::horizontal([Constraint::Min(1), Constraint::Length(hint.chars().count() as u16)]).areas(status_area);
    let bar = Style::new().bg(Color::Indexed(236));
    f.render_widget(Paragraph::new(left).style(bar), l);
    f.render_widget(Paragraph::new(hint).style(bar.fg(Color::Gray)), r);

    if let Mode::Picker(sel) = app.input.mode {
        draw_picker(f, sel);
    }
}

/// Draw the tab's panes at their rects, borders between them (the focused
/// pane's edges in the profile colour), and the focused pane's cursor.
fn draw_tab(app: &App, tab: &TabInfo, area: Rect, f: &mut Frame) {
    let visible: Vec<_> = tab.panes.iter().filter(|p| !tab.zoomed || p.id == tab.focused).collect();
    let to_screen = |r: layout::Rect| {
        Rect::new(area.x + r.x, area.y + r.y, r.cols, r.rows).intersection(area)
    };
    for p in &visible {
        if let Some(parser) = app.screens.get(&p.id) {
            render_screen(parser.screen(), to_screen(p.rect), f.buffer_mut());
        }
    }
    if visible.len() > 1 {
        let focused = tab.panes.iter().find(|p| p.id == tab.focused).map(|p| to_screen(p.rect));
        draw_borders(&visible.iter().map(|p| to_screen(p.rect)).collect::<Vec<_>>(), focused, color(&tab.color), area, f.buffer_mut());
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
fn draw_borders(panes: &[Rect], focused: Option<Rect>, accent: Color, area: Rect, buf: &mut Buffer) {
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
            let fg = if near_focus { accent } else { BORDER };
            buf[(x, y)].set_symbol(sym).set_style(Style::new().fg(fg));
        }
    }
}

fn draw_picker(f: &mut Frame, sel: usize) {
    let opts = picker_options();
    let h = opts.len() as u16 + 2;
    let area = f.area();
    let rect = Rect {
        x: area.width.saturating_sub(36) / 2,
        y: area.height.saturating_sub(h) / 2,
        width: 36.min(area.width),
        height: h.min(area.height),
    };
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
        .block(Block::new().borders(Borders::ALL).title(" new tab · profile "))
        .highlight_style(Style::new().add_modifier(Modifier::REVERSED));
    let mut state = ListState::default().with_selected(Some(sel));
    f.render_widget(Clear, rect);
    f.render_stateful_widget(list, rect, &mut state);
}

fn color(name: &str) -> Color {
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
