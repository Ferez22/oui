//! Right panel (`prefix b`; `prefix Tab` focuses it): connected accounts for
//! the active tab's profile, sessions, profiles — in `panel.widgets` order.
//!
//! Account detection is filesystem-only and fast, but still runs on a blocking
//! thread (never in the draw path); results come back as `Bg` events. While the
//! panel is visible it refreshes when the active profile changes, when it
//! opens, and every `REFRESH`.

use super::picker::OverlayStyle;
use crate::accounts::{self, LoginState, ToolStatus};
use crate::config::Widget;
use crate::profile::{self, ProfileConfig};
use crate::protocol::SessionInfo;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::UnboundedSender;

pub const REFRESH: Duration = Duration::from_secs(5);
/// Pane columns kept when the panel would take more.
const MIN_PANE_COLS: u16 = 20;
/// Width of the tool-name column in account rows ("supabase").
const TOOL_COL: usize = 8;

/// Result of a background refresh.
pub struct Bg {
    pub profile: String,
    pub tools: Result<Vec<ToolStatus>, String>,
    pub profiles: Vec<ProfileConfig>,
}

/// Columns the panel takes from a terminal `cols` wide (0 when hidden).
pub fn width(visible: bool, configured: u16, cols: u16) -> u16 {
    if !visible { 0 } else { configured.min(cols.saturating_sub(MIN_PANE_COLS)) }
}

/// What Enter does on a row.
#[derive(Debug, Clone, PartialEq)]
pub enum RowAction {
    /// Open a new tab in the panel's profile running `cmd` (if any); flash `hint`.
    Login { tool: &'static str, cmd: Option<String>, hint: Option<String> },
    Session(String),
    Profile(String),
}

pub struct Row {
    pub line: Line<'static>,
    pub action: Option<RowAction>,
}

impl Row {
    fn text(line: Line<'static>) -> Row {
        Row { line, action: None }
    }
}

/// How to log in to a tool from the panel: (command to run in a new tab, hint to flash).
pub fn login_command(t: &ToolStatus) -> (Option<String>, Option<String>) {
    match t.tool {
        // Whether `claude /login` starts the login flow is unverified; plain
        // `claude` always offers it (or accepts /login).
        "claude" => (Some("claude".into()), Some("claude: type /login to sign in for this profile".into())),
        // The "login" is an identity the user must type; never run it with a placeholder.
        "git" => (None, Some("git: run  git config --global user.email <you>  in this profile".into())),
        _ => (Some(t.login_cmd.to_string()), None),
    }
}

/// Account rows: installed tools (and any tool with a login, installed or
/// not) in `accounts::TOOLS` order, then a dim count of the hidden ones.
/// Not-installed tools without a login are noise: there's nothing to log in to.
pub fn account_rows(tools: &[ToolStatus]) -> Vec<Row> {
    let dim = Style::new().add_modifier(Modifier::DIM);
    let mut rows = Vec::new();
    let mut hidden = 0;
    for t in tools {
        if !t.installed && t.state != LoginState::LoggedIn {
            hidden += 1;
            continue;
        }
        let name = format!("{:<TOOL_COL$} ", t.tool);
        let line = match t.state {
            LoginState::LoggedIn => Line::from(vec![
                Span::styled("● ", Style::new().fg(Color::Green)),
                Span::raw(name),
                Span::raw(t.account.clone().unwrap_or_else(|| "logged in".into())),
            ]),
            LoginState::Unknown => Line::from(vec![
                Span::styled("? ", Style::new().fg(Color::Yellow)),
                Span::raw(name),
                Span::styled("can't tell", Style::new().fg(Color::Yellow)),
            ]),
            LoginState::LoggedOut => Line::from(vec![Span::raw("○ "), Span::raw(name), Span::raw("logged out")]).style(dim),
        };
        let (cmd, hint) = login_command(t);
        rows.push(Row { line, action: Some(RowAction::Login { tool: t.tool, cmd, hint }) });
    }
    if hidden > 0 {
        rows.push(Row::text(Line::styled(format!("  +{hidden} not installed"), dim)));
    }
    rows
}

/// Session rows; the current one is marked and shows the live tab count.
pub fn session_rows(sessions: &[SessionInfo], current: &str, current_tabs: usize) -> Vec<Row> {
    let w = sessions.iter().map(|s| s.name.chars().count()).max().unwrap_or(0);
    sessions
        .iter()
        .map(|s| {
            let cur = s.name == current;
            let tabs = if cur { current_tabs } else { s.tabs };
            let text = format!("{} {:<w$}  {tabs} tab{}", if cur { "▸" } else { " " }, s.name, if tabs == 1 { "" } else { "s" });
            let style = if cur { Style::new().add_modifier(Modifier::BOLD) } else { Style::new() };
            Row { line: Line::styled(text, style), action: Some(RowAction::Session(s.name.clone())) }
        })
        .collect()
}

pub fn profile_rows(profiles: &[ProfileConfig]) -> Vec<Row> {
    let w = profiles.iter().map(|p| p.name.chars().count()).max().unwrap_or(0);
    profiles
        .iter()
        .map(|p| Row {
            line: Line::from(vec![
                Span::styled("● ", Style::new().fg(super::render::color(&p.color))),
                Span::raw(format!("{:<w$}  ", p.name)),
                Span::styled(if p.shared { "shared" } else { "fresh" }, Style::new().add_modifier(Modifier::DIM)),
            ]),
            action: Some(RowAction::Profile(p.name.clone())),
        })
        .collect()
}

/// Accounts detected for one profile.
pub struct Accounts {
    pub profile: String,
    pub tools: Result<Vec<ToolStatus>, String>,
}

/// What the panel shows about the client's current place.
pub struct Here<'a> {
    pub profile: &'a str,
    pub profile_color: Color,
    pub session: &'a str,
    pub tabs: usize,
}

#[derive(Default)]
pub struct Panel {
    pub visible: bool,
    pub focused: bool,
    /// Index into the selectable rows.
    pub sel: usize,
    pub accounts: Option<Accounts>,
    pub sessions: Vec<SessionInfo>,
    pub profiles: Vec<ProfileConfig>,
    /// Profile whose detection is running.
    pending: Option<String>,
    last_refresh: Option<Instant>,
}

impl Panel {
    pub fn new(visible: bool) -> Self {
        Panel { visible, ..Default::default() }
    }

    /// Start a background refresh if one is due. Returns true when the
    /// session list should be re-requested too.
    pub fn refresh(&mut self, profile: &str, force: bool, tx: &UnboundedSender<Bg>) -> bool {
        if !self.visible || self.pending.is_some() {
            return false;
        }
        let stale = self.accounts.as_ref().is_none_or(|a| a.profile != profile);
        let due = self.last_refresh.is_none_or(|t| t.elapsed() >= REFRESH);
        if !(force || stale || due) {
            return false;
        }
        self.pending = Some(profile.to_string());
        self.last_refresh = Some(Instant::now());
        let (tx, profile) = (tx.clone(), profile.to_string());
        tokio::task::spawn_blocking(move || {
            let tools = accounts::detect(Some(&profile)).map_err(|e| format!("{e:#}"));
            let _ = tx.send(Bg { profile, tools, profiles: profile::list() });
        });
        true
    }

    pub fn on_bg(&mut self, bg: Bg) {
        self.pending = None;
        self.profiles = bg.profiles;
        self.accounts = Some(Accounts { profile: bg.profile, tools: bg.tools });
    }

    /// Every row in widget order (headers, blanks and items).
    pub fn rows(&self, widgets: &[Widget], here: &Here, accent: Color) -> Vec<Row> {
        let head = |t: &str| Span::styled(t.to_string(), Style::new().fg(accent).add_modifier(Modifier::BOLD));
        let dim = Style::new().add_modifier(Modifier::DIM);
        let mut rows = Vec::new();
        for (i, w) in widgets.iter().enumerate() {
            if i > 0 {
                rows.push(Row::text(Line::raw("")));
            }
            match w {
                Widget::Accounts => {
                    rows.push(Row::text(Line::from(vec![
                        head("ACCOUNTS · "),
                        Span::styled(here.profile.to_string(), Style::new().fg(here.profile_color).add_modifier(Modifier::BOLD)),
                    ])));
                    match &self.accounts {
                        Some(a) if a.profile == here.profile => match &a.tools {
                            Ok(t) => rows.extend(account_rows(t)),
                            Err(e) => rows.push(Row::text(Line::styled(format!("  {e}"), Style::new().fg(Color::Red)))),
                        },
                        _ => rows.push(Row::text(Line::styled("  checking…", dim))),
                    }
                }
                Widget::Sessions => {
                    rows.push(Row::text(Line::from(head("SESSIONS"))));
                    rows.extend(session_rows(&self.sessions, here.session, here.tabs));
                }
                Widget::Profiles => {
                    rows.push(Row::text(Line::from(head("PROFILES"))));
                    if self.profiles.is_empty() {
                        rows.push(Row::text(Line::styled("  none · oui profile new <name>", dim)));
                    }
                    rows.extend(profile_rows(&self.profiles));
                }
            }
        }
        rows
    }

    /// Action of the selected row (when focused).
    pub fn selected(&self, rows: Vec<Row>) -> Option<RowAction> {
        rows.into_iter().filter_map(|r| r.action).nth(self.sel)
    }

    /// Move the selection by `delta` over the selectable rows (wrapping).
    pub fn move_sel(&mut self, rows: &[Row], delta: isize) {
        let n = rows.iter().filter(|r| r.action.is_some()).count();
        if n == 0 {
            self.sel = 0;
            return;
        }
        self.sel = (self.sel.min(n - 1) as isize + delta).rem_euclid(n as isize) as usize;
    }

    pub fn draw(&self, f: &mut Frame, area: Rect, rows: Vec<Row>, st: &OverlayStyle, footer: &str) {
        if area.width < 2 || area.height == 0 {
            return;
        }
        let border = if self.focused { st.border } else { st.dim };
        let block = Block::new().borders(Borders::LEFT).border_style(border).style(st.base);
        let inner = block.inner(area);
        f.render_widget(block, area);
        let height = inner.height.saturating_sub(1) as usize; // last line: footer
        // Position of the selected row among all rows, to scroll it into view.
        let sel_row = self
            .focused
            .then(|| rows.iter().enumerate().filter(|(_, r)| r.action.is_some()).nth(self.sel).map(|(i, _)| i))
            .flatten();
        let skip = sel_row.map_or(0, |i| (i + 1).saturating_sub(height));
        let w = inner.width.saturating_sub(1) as usize;
        let mut lines: Vec<Line> = rows
            .into_iter()
            .enumerate()
            .skip(skip)
            .take(height)
            .map(|(i, r)| {
                let mut spans = vec![Span::raw(" ")];
                spans.extend(r.line.spans.iter().cloned());
                let line = Line::from(spans).style(r.line.style);
                if Some(i) == sel_row {
                    let pad = w.saturating_sub(line.width().saturating_sub(1));
                    let mut spans: Vec<Span> = line.spans.into_iter().map(|s| s.style(st.selected)).collect();
                    spans.push(Span::styled(" ".repeat(pad), st.selected));
                    Line::from(spans)
                } else {
                    line
                }
            })
            .collect();
        lines.resize(height, Line::raw(""));
        lines.push(Line::styled(format!(" {footer}"), st.dim));
        f.render_widget(Paragraph::new(lines).style(st.base), inner);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(tool: &'static str, installed: bool, state: LoginState, account: Option<&str>) -> ToolStatus {
        ToolStatus { tool, installed, state, account: account.map(Into::into), login_cmd: "x login" }
    }

    fn texts(rows: &[Row]) -> Vec<String> {
        rows.iter().map(|r| r.line.to_string()).collect()
    }

    #[test]
    fn account_rows_format() {
        let tools = [
            tool("claude", true, LoginState::Unknown, None),
            tool("railway", true, LoginState::LoggedIn, None),
            tool("gh", true, LoginState::LoggedIn, Some("test-user")),
            tool("vercel", true, LoginState::LoggedOut, None),
            tool("aws", false, LoginState::LoggedOut, None),
            tool("fly", false, LoginState::Unknown, None),
            tool("docker", false, LoginState::LoggedIn, Some("me")),
        ];
        let rows = account_rows(&tools);
        assert_eq!(texts(&rows), [
            "? claude   can't tell",
            "● railway  logged in",
            "● gh       test-user",
            "○ vercel   logged out",
            "● docker   me",
            "  +2 not installed",
        ]);
        // Unknown is never shown as logged out, and has its own colour.
        assert_eq!(rows[0].line.spans[0].style.fg, Some(Color::Yellow));
        assert_eq!(rows[1].line.spans[0].style.fg, Some(Color::Green));
        assert!(rows[3].line.style.add_modifier.contains(Modifier::DIM));
        assert!(rows[5].action.is_none());
        assert_eq!(rows[1].action, Some(RowAction::Login { tool: "railway", cmd: Some("x login".into()), hint: None }));
        let RowAction::Login { cmd, hint, .. } = rows[0].action.clone().unwrap() else { panic!() };
        assert_eq!(cmd.as_deref(), Some("claude"));
        assert!(hint.unwrap().contains("/login"));
    }

    #[test]
    fn git_login_is_a_hint_only() {
        let (cmd, hint) = login_command(&tool("git", true, LoginState::LoggedOut, None));
        assert_eq!(cmd, None);
        assert!(hint.unwrap().contains("user.email"));
    }

    #[test]
    fn panel_rows_and_selection() {
        let mut p = Panel::new(true);
        p.sessions = vec![
            SessionInfo { name: "main".into(), tabs: 1, clients: 1 },
            SessionInfo { name: "side".into(), tabs: 2, clients: 0 },
        ];
        p.profiles = vec![
            ProfileConfig { name: "work".into(), color: "green".into(), shared: true, extra_shared: vec![], keychain: Default::default() },
            ProfileConfig { name: "personal".into(), color: "blue".into(), shared: false, extra_shared: vec![], keychain: Default::default() },
        ];
        let here = Here { profile: "work", profile_color: Color::Green, session: "main", tabs: 3 };
        let widgets = [Widget::Accounts, Widget::Sessions, Widget::Profiles];
        assert_eq!(texts(&p.rows(&widgets, &here, Color::Yellow)), [
            "ACCOUNTS · work",
            "  checking…",
            "",
            "SESSIONS",
            "▸ main  3 tabs",
            "  side  2 tabs",
            "",
            "PROFILES",
            "● work      shared",
            "● personal  fresh",
        ]);
        // Results for another profile are stale.
        p.accounts = Some(Accounts { profile: "personal".into(), tools: Ok(vec![]) });
        assert_eq!(texts(&p.rows(&widgets, &here, Color::Yellow))[1], "  checking…");
        p.accounts = Some(Accounts { profile: "work".into(), tools: Ok(vec![tool("gh", true, LoginState::LoggedIn, Some("u"))]) });
        let rows = p.rows(&[Widget::Profiles, Widget::Accounts], &here, Color::Yellow);
        assert_eq!(texts(&rows)[4..], ["ACCOUNTS · work", "● gh       u"]);

        p.move_sel(&rows, -1);
        assert_eq!(p.sel, 2);
        assert!(matches!(p.selected(p.rows(&[Widget::Profiles, Widget::Accounts], &here, Color::Yellow)), Some(RowAction::Login { tool: "gh", .. })));
        p.move_sel(&rows, 1);
        assert_eq!(p.selected(rows), Some(RowAction::Profile("work".into())));
    }

    #[test]
    fn panel_width_leaves_room_for_panes() {
        assert_eq!(width(false, 32, 120), 0);
        assert_eq!(width(true, 32, 120), 32);
        assert_eq!(width(true, 32, 40), 20);
        assert_eq!(width(true, 32, 10), 0);
    }
}
