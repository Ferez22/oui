//! User config: `~/.oui/config.toml` (keys, theme, status bar, panel, general).
//!
//! Every section and key is optional; anything missing takes its default.
//! Parse errors carry the TOML line/column so they can be shown as-is.

use crate::profile;
use anyhow::{Context, Result, anyhow, bail};
use ratatui::style::Color;
use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer};
use std::collections::BTreeMap;
use std::fmt::{self, Write as _};
use std::path::{Path, PathBuf};

/// `~/.oui/config.toml`.
pub fn path() -> Result<PathBuf> {
    Ok(profile::oui_root()?.join("config.toml"))
}

/// User theme files: `~/.oui/themes/<name>.toml`.
pub fn themes_dir() -> Result<PathBuf> {
    Ok(profile::oui_root()?.join("themes"))
}

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub general: General,
    pub keys: Keys,
    pub theme: Theme,
    pub status: Status,
    pub panel: Panel,
}

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct RawConfig {
    general: General,
    keys: Keys,
    theme: ThemeSpec,
    status: Status,
    panel: Panel,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            general: General::default(),
            keys: Keys::default(),
            theme: Theme::builtin("default").expect("built-in default theme"),
            status: Status::default(),
            panel: Panel::default(),
        }
    }
}

impl Config {
    /// Load the config file. A missing file gives the defaults; an invalid one is an error.
    pub fn load() -> Result<Config> {
        let path = path()?;
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Config::default()),
            Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
        };
        Config::parse(&text, Some(&themes_dir()?)).with_context(|| format!("invalid config {}", path.display()))
    }

    /// Like `load`, but falls back to the defaults and returns a warning to show the user.
    pub fn load_or_default() -> (Config, Option<String>) {
        match Config::load() {
            Ok(c) => (c, None),
            Err(e) => (Config::default(), Some(format!("{e:#}\nusing default config"))),
        }
    }

    /// Parse config text; named themes not built in are read from `themes_dir`.
    pub fn parse(text: &str, themes_dir: Option<&Path>) -> Result<Config> {
        let raw: RawConfig = toml::from_str(text)?;
        Ok(Config {
            general: raw.general,
            keys: raw.keys,
            theme: raw.theme.resolve(themes_dir)?,
            status: raw.status,
            panel: raw.panel,
        })
    }
}

// ---------------------------------------------------------------------------
// [general]

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct General {
    /// Profile for new sessions/tabs when none is given (None = system).
    pub default_profile: Option<String>,
    /// Shell to spawn instead of `$SHELL`.
    pub shell: Option<String>,
    /// Scrollback lines kept per pane.
    pub scrollback: usize,
    pub mouse: bool,
}

impl Default for General {
    fn default() -> Self {
        General { default_profile: None, shell: None, scrollback: 10_000, mouse: true }
    }
}

// ---------------------------------------------------------------------------
// [keys]

/// Something a key binding can do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(try_from = "String")]
pub enum Action {
    NewTab,
    NextTab,
    PrevTab,
    /// Jump to tab 1..=9 (`select-tab-N`).
    SelectTab(u8),
    ClosePane,
    CloseTab,
    Detach,
    SplitRight,
    SplitDown,
    FocusLeft,
    FocusRight,
    FocusUp,
    FocusDown,
    ResizeLeft,
    ResizeRight,
    ResizeUp,
    ResizeDown,
    Zoom,
    TogglePanel,
    /// Move keyboard focus to the right panel (opening it if hidden).
    FocusPanel,
    CopyMode,
    /// Reserved (v2).
    CommandPalette,
    /// Send the prefix key itself to the pane.
    SendPrefix,
    RenameTab,
    SessionPicker,
}

const ACTION_NAMES: &[(&str, Action)] = &[
    ("new-tab", Action::NewTab),
    ("next-tab", Action::NextTab),
    ("prev-tab", Action::PrevTab),
    ("close-pane", Action::ClosePane),
    ("close-tab", Action::CloseTab),
    ("detach", Action::Detach),
    ("split-right", Action::SplitRight),
    ("split-down", Action::SplitDown),
    ("focus-left", Action::FocusLeft),
    ("focus-right", Action::FocusRight),
    ("focus-up", Action::FocusUp),
    ("focus-down", Action::FocusDown),
    ("resize-left", Action::ResizeLeft),
    ("resize-right", Action::ResizeRight),
    ("resize-up", Action::ResizeUp),
    ("resize-down", Action::ResizeDown),
    ("zoom", Action::Zoom),
    ("toggle-panel", Action::TogglePanel),
    ("focus-panel", Action::FocusPanel),
    ("copy-mode", Action::CopyMode),
    ("command-palette", Action::CommandPalette),
    ("send-prefix", Action::SendPrefix),
    ("rename-tab", Action::RenameTab),
    ("session-picker", Action::SessionPicker),
];

impl std::str::FromStr for Action {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        if let Some(n) = s.strip_prefix("select-tab-")
            && let Ok(n @ 1..=9) = n.parse::<u8>()
        {
            return Ok(Action::SelectTab(n));
        }
        ACTION_NAMES
            .iter()
            .find(|(name, _)| *name == s)
            .map(|(_, a)| *a)
            .ok_or_else(|| format!("unknown action '{s}'"))
    }
}

impl TryFrom<String> for Action {
    type Error = String;
    fn try_from(s: String) -> Result<Self, String> {
        s.parse()
    }
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Action::SelectTab(n) = self {
            return write!(f, "select-tab-{n}");
        }
        let name = ACTION_NAMES.iter().find(|(_, a)| a == self).map(|(n, _)| *n).unwrap_or("?");
        f.write_str(name)
    }
}

/// A key in tmux-like notation ("C-b", "M-x", "Up", "%") and the byte
/// sequences a terminal in raw mode sends for it (several for keys like arrows).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Key {
    pub notation: String,
    pub seqs: Vec<Vec<u8>>,
}

impl Key {
    pub fn parse(notation: &str) -> Result<Key> {
        Ok(Key { notation: notation.to_string(), seqs: key_bytes(notation)? })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Binding {
    pub key: Key,
    pub action: Action,
}

/// Prefix key plus the bindings active after it. Pressing the prefix twice
/// sends it to the pane (`send-prefix`) unless the prefix key is bound explicitly.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(try_from = "RawKeys")]
pub struct Keys {
    pub prefix: Key,
    /// Sorted by notation.
    pub bindings: Vec<Binding>,
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawKeys {
    prefix: String,
    /// Layered over the defaults; the action "none" removes a default binding.
    bindings: BTreeMap<String, String>,
}

impl Default for RawKeys {
    fn default() -> Self {
        RawKeys { prefix: "C-b".into(), bindings: BTreeMap::new() }
    }
}

/// Default bindings after the prefix (tmux style).
pub const DEFAULT_BINDINGS: &[(&str, &str)] = &[
    ("c", "new-tab"),
    ("n", "next-tab"),
    ("p", "prev-tab"),
    ("1", "select-tab-1"),
    ("2", "select-tab-2"),
    ("3", "select-tab-3"),
    ("4", "select-tab-4"),
    ("5", "select-tab-5"),
    ("6", "select-tab-6"),
    ("7", "select-tab-7"),
    ("8", "select-tab-8"),
    ("9", "select-tab-9"),
    ("x", "close-pane"),
    ("&", "close-tab"),
    ("d", "detach"),
    ("%", "split-right"),
    ("|", "split-right"),
    ("\"", "split-down"),
    ("-", "split-down"),
    ("Left", "focus-left"),
    ("Right", "focus-right"),
    ("Up", "focus-up"),
    ("Down", "focus-down"),
    ("h", "focus-left"),
    ("l", "focus-right"),
    ("k", "focus-up"),
    ("j", "focus-down"),
    ("H", "resize-left"),
    ("L", "resize-right"),
    ("K", "resize-up"),
    ("J", "resize-down"),
    ("z", "zoom"),
    ("b", "toggle-panel"),
    ("Tab", "focus-panel"),
    ("[", "copy-mode"),
    ("s", "session-picker"),
    (",", "rename-tab"),
];

impl TryFrom<RawKeys> for Keys {
    type Error = anyhow::Error;
    fn try_from(raw: RawKeys) -> Result<Self> {
        let prefix = Key::parse(&raw.prefix).context("keys.prefix")?;
        // Keyed by primary byte sequence so "C-m" overrides "Enter", etc.
        let mut map: BTreeMap<Vec<u8>, Binding> = BTreeMap::new();
        map.insert(prefix.seqs[0].clone(), Binding { key: prefix.clone(), action: Action::SendPrefix });
        let user = raw.bindings.iter().map(|(k, a)| (k.as_str(), a.as_str()));
        for (notation, action) in DEFAULT_BINDINGS.iter().copied().chain(user) {
            let key = Key::parse(notation).with_context(|| format!("keys.bindings: key '{notation}'"))?;
            let seq = key.seqs[0].clone();
            if action == "none" {
                map.remove(&seq);
                continue;
            }
            let action = action.parse().map_err(|e| anyhow!("keys.bindings.'{notation}': {e}"))?;
            map.insert(seq, Binding { key, action });
        }
        let mut bindings: Vec<_> = map.into_values().collect();
        bindings.sort_by(|a, b| a.key.notation.cmp(&b.key.notation));
        Ok(Keys { prefix, bindings })
    }
}

impl Default for Keys {
    fn default() -> Self {
        RawKeys::default().try_into().expect("default key bindings")
    }
}

impl Keys {
    /// Length of the prefix key at the start of `bytes`, if it's there.
    pub fn match_prefix(&self, bytes: &[u8]) -> Option<usize> {
        self.prefix.seqs.iter().filter(|s| bytes.starts_with(s)).map(Vec::len).max()
    }

    /// The bound action at the start of `bytes` (input after the prefix) and how
    /// many bytes it consumed. Longest match wins, so `Esc` doesn't eat `Up`.
    pub fn lookup(&self, bytes: &[u8]) -> Option<(Action, usize)> {
        self.bindings
            .iter()
            .flat_map(|b| b.key.seqs.iter().map(move |s| (b.action, s)))
            .filter(|(_, s)| bytes.starts_with(s))
            .max_by_key(|(_, s)| s.len())
            .map(|(a, s)| (a, s.len()))
    }

    /// The key bound to `action`, for hints (first in notation order).
    pub fn key_for(&self, action: Action) -> Option<&str> {
        self.bindings.iter().find(|b| b.action == action).map(|b| b.key.notation.as_str())
    }
}

/// Bytes a terminal sends in raw mode for a key in tmux-like notation:
/// optional `C-`, `M-`, `S-` modifiers, then a single character or a key name
/// (Enter, Tab, BTab, Esc, Space, BSpace, Up, Down, Left, Right, Home, End,
/// PageUp/PPage, PageDown/NPage, Insert/IC, Delete/DC, F1-F12).
pub fn key_bytes(notation: &str) -> Result<Vec<Vec<u8>>> {
    let (mut ctrl, mut meta, mut shift) = (false, false, false);
    let mut rest = notation;
    // A modifier needs something after it, so "-" and "C--" still work.
    while rest.len() > 2 {
        match rest.get(..2) {
            Some("C-" | "c-") => ctrl = true,
            Some("M-" | "m-") => meta = true,
            Some("S-" | "s-") => shift = true,
            _ => break,
        }
        rest = &rest[2..];
    }
    if rest.is_empty() {
        bail!("empty key '{notation}'");
    }
    let csi_mod = 1 + shift as u8 + 2 * meta as u8 + 4 * ctrl as u8;

    let mut chars = rest.chars();
    let (first, single) = (chars.next().unwrap(), chars.next().is_none());
    if single {
        let mut c = first;
        if shift {
            c = c.to_ascii_uppercase();
        }
        let mut out = if ctrl { vec![ctrl_byte(c).with_context(|| format!("no Ctrl form for '{notation}'"))?] }
            else { c.to_string().into_bytes() };
        if meta {
            out.insert(0, 0x1b);
        }
        return Ok(vec![out]);
    }

    let lower = rest.to_ascii_lowercase();
    // Cursor keys: normal and application mode; modified form ESC [ 1 ; m X.
    let cursor = match lower.as_str() {
        "up" => Some(b'A'),
        "down" => Some(b'B'),
        "right" => Some(b'C'),
        "left" => Some(b'D'),
        "home" => Some(b'H'),
        "end" => Some(b'F'),
        _ => None,
    };
    if let Some(x) = cursor {
        return Ok(if csi_mod == 1 {
            vec![vec![0x1b, b'[', x], vec![0x1b, b'O', x]]
        } else {
            let mut v = vec![format!("\x1b[1;{csi_mod}").into_bytes()];
            v[0].push(x);
            if csi_mod == 3 {
                // Some terminals send Alt+arrow as ESC + arrow.
                v.push(vec![0x1b, 0x1b, b'[', x]);
            }
            v
        });
    }
    // Keys sent as ESC [ n ~ (modified: ESC [ n ; m ~).
    let tilde = match lower.as_str() {
        "insert" | "ic" => Some(2),
        "delete" | "dc" => Some(3),
        "pageup" | "ppage" => Some(5),
        "pagedown" | "npage" => Some(6),
        "f5" => Some(15),
        "f6" => Some(17),
        "f7" => Some(18),
        "f8" => Some(19),
        "f9" => Some(20),
        "f10" => Some(21),
        "f11" => Some(23),
        "f12" => Some(24),
        _ => None,
    };
    if let Some(n) = tilde {
        let s = if csi_mod == 1 { format!("\x1b[{n}~") } else { format!("\x1b[{n};{csi_mod}~") };
        return Ok(vec![s.into_bytes()]);
    }
    if let Some(x) = match lower.as_str() {
        "f1" => Some(b'P'),
        "f2" => Some(b'Q'),
        "f3" => Some(b'R'),
        "f4" => Some(b'S'),
        _ => None,
    } {
        let mut s = if csi_mod == 1 { vec![0x1b, b'O'] } else { format!("\x1b[1;{csi_mod}").into_bytes() };
        s.push(x);
        return Ok(vec![s]);
    }
    let base: &[u8] = match lower.as_str() {
        "enter" | "return" => b"\r",
        "tab" if shift => b"\x1b[Z",
        "tab" => b"\t",
        "btab" => b"\x1b[Z",
        "esc" | "escape" => b"\x1b",
        "space" if ctrl => b"\0",
        "space" => b" ",
        "bspace" | "backspace" => b"\x7f",
        _ => bail!("unknown key '{notation}'"),
    };
    let mut out = base.to_vec();
    if meta {
        out.insert(0, 0x1b);
    }
    Ok(vec![out])
}

fn ctrl_byte(c: char) -> Option<u8> {
    match c {
        'a'..='z' => Some(c as u8 - b'a' + 1),
        'A'..='Z' => Some(c as u8 - b'A' + 1),
        '@' | ' ' | '2' => Some(0),
        '[' | '3' => Some(0x1b),
        '\\' | '4' => Some(0x1c),
        ']' | '5' => Some(0x1d),
        '^' | '6' => Some(0x1e),
        '_' | '-' | '7' => Some(0x1f),
        '?' | '8' => Some(0x7f),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// [theme]

/// A theme colour: a terminal colour, or the colour of the pane's profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeColor {
    Color(Color),
    Profile,
}

impl ThemeColor {
    pub fn to_color(self, profile: Color) -> Color {
        match self {
            ThemeColor::Color(c) => c,
            ThemeColor::Profile => profile,
        }
    }
}

impl std::str::FromStr for ThemeColor {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s.to_ascii_lowercase().as_str() {
            "profile" => Ok(ThemeColor::Profile),
            "default" | "reset" | "none" => Ok(ThemeColor::Color(Color::Reset)),
            _ => s.parse().map(ThemeColor::Color).map_err(|_| {
                format!("invalid colour '{s}' (use a name like \"green\", \"#rrggbb\", 0-255, \"default\" or \"profile\")")
            }),
        }
    }
}

impl fmt::Display for ThemeColor {
    /// TOML value form (quoted string or bare index).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ThemeColor::Profile => f.write_str("\"profile\""),
            ThemeColor::Color(Color::Reset) => f.write_str("\"default\""),
            ThemeColor::Color(Color::Indexed(i)) => write!(f, "{i}"),
            ThemeColor::Color(Color::Rgb(r, g, b)) => write!(f, "\"#{r:02x}{g:02x}{b:02x}\""),
            // Named colours: "LightRed" -> "light-red".
            ThemeColor::Color(c) => {
                let mut name = String::new();
                for (i, ch) in c.to_string().chars().enumerate() {
                    if ch.is_ascii_uppercase() && i > 0 {
                        name.push('-');
                    }
                    name.push(ch.to_ascii_lowercase());
                }
                write!(f, "\"{name}\"")
            }
        }
    }
}

impl<'de> Deserialize<'de> for ThemeColor {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl Visitor<'_> for V {
            type Value = ThemeColor;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a colour name, \"#rrggbb\", 0-255, \"default\" or \"profile\"")
            }
            fn visit_str<E: de::Error>(self, s: &str) -> Result<ThemeColor, E> {
                s.parse().map_err(E::custom)
            }
            fn visit_i64<E: de::Error>(self, n: i64) -> Result<ThemeColor, E> {
                u8::try_from(n).map(|i| ThemeColor::Color(Color::Indexed(i))).map_err(|_| E::custom("colour index must be 0-255"))
            }
        }
        d.deserialize_any(V)
    }
}

/// Declares the theme fields once: the resolved `Theme`, the partial
/// `ThemeTable` used by config/theme files, and the field list.
macro_rules! theme_fields {
    ($($field:ident: $doc:literal),* $(,)?) => {
        #[derive(Debug, Clone, PartialEq)]
        pub struct Theme {
            $(#[doc = $doc] pub $field: ThemeColor,)*
        }

        #[derive(Debug, Default, Deserialize)]
        #[serde(default, deny_unknown_fields)]
        struct ThemeTable {
            /// Theme to start from (built-in or `~/.oui/themes/<name>.toml`).
            base: Option<String>,
            $($field: Option<ThemeColor>,)*
        }

        impl Theme {
            fn apply(&mut self, t: &ThemeTable) {
                $(if let Some(c) = t.$field { self.$field = c; })*
            }

            /// (field name, description, value) for every colour.
            pub fn fields(&self) -> Vec<(&'static str, &'static str, ThemeColor)> {
                vec![$((stringify!($field), $doc, self.$field),)*]
            }
        }
    };
}

theme_fields! {
    tab_bar_bg: "Tab bar background",
    tab_bar_fg: "Tab bar text",
    tab_active_bg: "Active tab background",
    tab_active_fg: "Active tab text",
    status_bg: "Status bar background",
    status_fg: "Status bar text",
    border: "Pane borders",
    focused_border: "Border of the focused pane",
    panel_bg: "Right panel background",
    panel_fg: "Right panel text",
    accent: "Highlights (prefix indicator, selection)",
}

pub const BUILTIN_THEMES: &[&str] = &["default", "nord", "light"];

impl Theme {
    pub fn builtin(name: &str) -> Option<Theme> {
        use ThemeColor::{Color as C, Profile};
        let rgb = |r, g, b| C(Color::Rgb(r, g, b));
        Some(match name {
            "default" => Theme {
                tab_bar_bg: C(Color::Reset),
                tab_bar_fg: C(Color::Reset),
                tab_active_bg: Profile,
                tab_active_fg: C(Color::Black),
                status_bg: C(Color::Indexed(236)),
                status_fg: C(Color::Gray),
                border: C(Color::Indexed(240)),
                focused_border: Profile,
                panel_bg: C(Color::Indexed(235)),
                panel_fg: C(Color::Gray),
                accent: C(Color::Yellow),
            },
            "nord" => Theme {
                tab_bar_bg: rgb(0x2e, 0x34, 0x40),
                tab_bar_fg: rgb(0xd8, 0xde, 0xe9),
                tab_active_bg: Profile,
                tab_active_fg: rgb(0x2e, 0x34, 0x40),
                status_bg: rgb(0x3b, 0x42, 0x52),
                status_fg: rgb(0xe5, 0xe9, 0xf0),
                border: rgb(0x4c, 0x56, 0x6a),
                focused_border: Profile,
                panel_bg: rgb(0x2e, 0x34, 0x40),
                panel_fg: rgb(0xd8, 0xde, 0xe9),
                accent: rgb(0x88, 0xc0, 0xd0),
            },
            "light" => Theme {
                tab_bar_bg: rgb(0xee, 0xee, 0xee),
                tab_bar_fg: rgb(0x44, 0x44, 0x44),
                tab_active_bg: Profile,
                tab_active_fg: rgb(0xff, 0xff, 0xff),
                status_bg: rgb(0xdd, 0xdd, 0xdd),
                status_fg: rgb(0x22, 0x22, 0x22),
                border: rgb(0xbb, 0xbb, 0xbb),
                focused_border: Profile,
                panel_bg: rgb(0xf5, 0xf5, 0xf5),
                panel_fg: rgb(0x22, 0x22, 0x22),
                accent: rgb(0x00, 0x66, 0xcc),
            },
            _ => return None,
        })
    }

    /// A named theme: a user file in `themes_dir` (layered over a built-in
    /// base) takes precedence over a built-in of the same name.
    pub fn named(name: &str, themes_dir: Option<&Path>) -> Result<Theme> {
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            bail!("theme name must be letters, digits, '-' or '_'");
        }
        let file = themes_dir.map(|d| d.join(format!("{name}.toml"))).filter(|f| f.exists());
        let Some(file) = file else {
            return Theme::builtin(name).with_context(|| {
                format!("unknown theme '{name}' (built-in: {}; or add ~/.oui/themes/{name}.toml)", BUILTIN_THEMES.join(", "))
            });
        };
        let text = std::fs::read_to_string(&file)?;
        let table: ThemeTable = toml::from_str(&text).with_context(|| format!("invalid theme {}", file.display()))?;
        let base = table.base.as_deref().unwrap_or("default");
        let mut theme = Theme::builtin(base)
            .with_context(|| format!("{}: base must be a built-in theme ({})", file.display(), BUILTIN_THEMES.join(", ")))?;
        theme.apply(&table);
        Ok(theme)
    }
}

/// `theme = "name"` or a `[theme]` table with optional `base` and overrides.
#[derive(Default)]
struct ThemeSpec(ThemeTable);

impl ThemeSpec {
    fn resolve(&self, themes_dir: Option<&Path>) -> Result<Theme> {
        let mut theme = Theme::named(self.0.base.as_deref().unwrap_or("default"), themes_dir)?;
        theme.apply(&self.0);
        Ok(theme)
    }
}

impl<'de> Deserialize<'de> for ThemeSpec {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = ThemeSpec;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a theme name or a [theme] table")
            }
            fn visit_str<E: de::Error>(self, s: &str) -> Result<ThemeSpec, E> {
                Ok(ThemeSpec(ThemeTable { base: Some(s.into()), ..Default::default() }))
            }
            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<ThemeSpec, A::Error> {
                ThemeTable::deserialize(de::value::MapAccessDeserializer::new(map)).map(ThemeSpec)
            }
        }
        d.deserialize_any(V)
    }
}

// ---------------------------------------------------------------------------
// [status]

/// A status bar segment. Rendering is up to the client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    Session,
    Profile,
    Mode,
    Hints,
    Clock,
    Cwd,
    Hostname,
    TabsCount,
    /// `{ text = "…" }`
    Text(String),
}

const SEGMENT_NAMES: &[(&str, Segment)] = &[
    ("session", Segment::Session),
    ("profile", Segment::Profile),
    ("mode", Segment::Mode),
    ("hints", Segment::Hints),
    ("clock", Segment::Clock),
    ("cwd", Segment::Cwd),
    ("hostname", Segment::Hostname),
    ("tabs-count", Segment::TabsCount),
];

impl fmt::Display for Segment {
    /// TOML value form.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Segment::Text(t) => write!(f, "{{ text = {} }}", toml_str(t)),
            s => {
                let name = SEGMENT_NAMES.iter().find(|(_, v)| v == s).map(|(n, _)| *n).unwrap_or("?");
                write!(f, "\"{name}\"")
            }
        }
    }
}

impl<'de> Deserialize<'de> for Segment {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Text {
            text: String,
        }
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Segment;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a segment name or { text = \"…\" }")
            }
            fn visit_str<E: de::Error>(self, s: &str) -> Result<Segment, E> {
                SEGMENT_NAMES.iter().find(|(n, _)| *n == s).map(|(_, v)| v.clone()).ok_or_else(|| {
                    let names: Vec<_> = SEGMENT_NAMES.iter().map(|(n, _)| *n).collect();
                    E::custom(format!("unknown segment '{s}' (one of: {})", names.join(", ")))
                })
            }
            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Segment, A::Error> {
                Text::deserialize(de::value::MapAccessDeserializer::new(map)).map(|t| Segment::Text(t.text))
            }
        }
        d.deserialize_any(V)
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Status {
    pub left: Vec<Segment>,
    pub right: Vec<Segment>,
}

impl Default for Status {
    fn default() -> Self {
        Status {
            left: vec![Segment::Mode, Segment::Session, Segment::Profile],
            right: vec![Segment::Hints, Segment::Clock],
        }
    }
}

// ---------------------------------------------------------------------------
// [panel]

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Widget {
    Accounts,
    Sessions,
    Profiles,
}

impl Widget {
    fn name(self) -> &'static str {
        match self {
            Widget::Accounts => "accounts",
            Widget::Sessions => "sessions",
            Widget::Profiles => "profiles",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Panel {
    pub visible: bool,
    pub width: u16,
    pub widgets: Vec<Widget>,
}

impl Default for Panel {
    fn default() -> Self {
        Panel { visible: false, width: 32, widgets: vec![Widget::Accounts, Widget::Sessions, Widget::Profiles] }
    }
}

// ---------------------------------------------------------------------------
// Default config text

fn toml_str(s: &str) -> String {
    toml::Value::String(s.into()).to_string()
}

fn toml_key(k: &str) -> String {
    if !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        k.into()
    } else {
        toml_str(k)
    }
}

/// The default config as commented TOML (`oui config default`).
pub fn default_toml() -> String {
    let c = Config::default();
    let list = |v: Vec<String>| format!("[{}]", v.join(", "));
    let mut s = String::new();
    let w = &mut s;
    let _ = writeln!(w, "# oUI config: ~/.oui/config.toml");
    let _ = writeln!(w, "# Every key is optional; missing keys use the defaults shown here.\n");

    let g = &c.general;
    let _ = writeln!(w, "[general]");
    let _ = writeln!(w, "# Profile for new sessions and tabs when none is given (unset = system).");
    let _ = writeln!(w, "# default_profile = \"work\"");
    let _ = writeln!(w, "# Shell to run in panes (default: $SHELL).");
    let _ = writeln!(w, "# shell = \"/bin/zsh\"");
    let _ = writeln!(w, "# Lines of scrollback kept per pane.");
    let _ = writeln!(w, "scrollback = {}", g.scrollback);
    let _ = writeln!(w, "# Mouse support (click tabs/panes, scroll).");
    let _ = writeln!(w, "mouse = {}\n", g.mouse);

    let _ = writeln!(w, "[keys]");
    let _ = writeln!(w, "# Prefix key. Notation: C- (Ctrl), M- (Alt/Meta), S- (Shift), then a character");
    let _ = writeln!(w, "# or a key name: Enter Tab BTab Esc Space BSpace Up Down Left Right Home End");
    let _ = writeln!(w, "# PageUp PageDown Insert Delete F1-F12. Pressing the prefix twice sends it");
    let _ = writeln!(w, "# to the pane (send-prefix).");
    let _ = writeln!(w, "prefix = {}\n", toml_str(&c.keys.prefix.notation));
    let _ = writeln!(w, "# Keys pressed after the prefix. Entries here are layered over the defaults;");
    let _ = writeln!(w, "# use \"none\" to remove a default binding. Actions: new-tab next-tab prev-tab");
    let _ = writeln!(w, "# select-tab-1..9 close-pane close-tab detach split-right split-down");
    let _ = writeln!(w, "# focus-left/right/up/down resize-left/right/up/down zoom toggle-panel");
    let _ = writeln!(w, "# focus-panel copy-mode command-palette send-prefix rename-tab session-picker");
    let _ = writeln!(w, "[keys.bindings]");
    for (k, a) in DEFAULT_BINDINGS {
        let _ = writeln!(w, "{} = {}", toml_key(k), toml_str(a));
    }

    let _ = writeln!(w, "\n# Colours: a name (\"green\", \"light-blue\", \"dark-gray\"), \"#rrggbb\", a 0-255 index,");
    let _ = writeln!(w, "# \"default\" (terminal default) or \"profile\" (the pane's profile colour).");
    let _ = writeln!(w, "# Shorthand: theme = \"nord\" (built-in: {}; or ~/.oui/themes/<name>.toml,", BUILTIN_THEMES.join(", "));
    let _ = writeln!(w, "# which holds the same keys as below and may set its own built-in base).");
    let _ = writeln!(w, "[theme]");
    let _ = writeln!(w, "base = \"default\"");
    let _ = writeln!(w, "# Overrides on top of the base theme (values shown are the default theme's):");
    for (name, doc, value) in c.theme.fields() {
        let _ = writeln!(w, "# {:<28} # {doc}", format!("{name} = {value}"));
    }

    let seg = |v: &[Segment]| list(v.iter().map(Segment::to_string).collect());
    let _ = writeln!(w, "\n# Status bar segments: session profile mode hints clock cwd hostname tabs-count,");
    let _ = writeln!(w, "# or custom text: {{ text = \"…\" }}");
    let _ = writeln!(w, "[status]");
    let _ = writeln!(w, "left = {}", seg(&c.status.left));
    let _ = writeln!(w, "right = {}\n", seg(&c.status.right));

    let p = &c.panel;
    let _ = writeln!(w, "# Right panel (toggle-panel key; focus-panel moves the keyboard into it).");
    let _ = writeln!(w, "# Widgets, top to bottom: accounts sessions profiles");
    let _ = writeln!(w, "[panel]");
    let _ = writeln!(w, "visible = {}", p.visible);
    let _ = writeln!(w, "width = {}", p.width);
    let _ = writeln!(w, "widgets = {}", list(p.widgets.iter().map(|x| toml_str(x.name())).collect()));
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_round_trip() {
        let parsed = Config::parse(&default_toml(), None).unwrap();
        assert_eq!(parsed, Config::default());
        assert_eq!(Config::parse("", None).unwrap(), Config::default());
    }

    #[test]
    fn example_file_is_current() {
        let file = include_str!("../docs/config.example.toml");
        assert_eq!(file, default_toml(), "regenerate: oui config default > docs/config.example.toml");
    }

    #[test]
    fn partial_config_keeps_defaults() {
        let c = Config::parse("[general]\nscrollback = 500\n[panel]\nwidth = 40", None).unwrap();
        assert_eq!(c.general.scrollback, 500);
        assert!(c.general.mouse);
        assert_eq!(c.panel.width, 40);
        assert_eq!(c.panel.widgets.len(), 3);
        assert_eq!(c.keys, Keys::default());
    }

    #[test]
    fn key_notation() {
        let k = |s| key_bytes(s).unwrap();
        assert_eq!(k("C-b"), vec![vec![0x02]]);
        assert_eq!(k("C-B"), vec![vec![0x02]]);
        assert_eq!(k("C-a"), vec![vec![0x01]]);
        assert_eq!(k("C-Space"), vec![vec![0x00]]);
        assert_eq!(k("M-x"), vec![b"\x1bx".to_vec()]);
        assert_eq!(k("M-C-b"), vec![vec![0x1b, 0x02]]);
        assert_eq!(k("%"), vec![b"%".to_vec()]);
        assert_eq!(k("-"), vec![b"-".to_vec()]);
        assert_eq!(k("C--"), vec![vec![0x1f]]);
        assert_eq!(k("H"), vec![b"H".to_vec()]);
        assert_eq!(k("S-h"), vec![b"H".to_vec()]);
        assert_eq!(k("Enter"), vec![b"\r".to_vec()]);
        assert_eq!(k("Esc"), vec![b"\x1b".to_vec()]);
        assert_eq!(k("Up"), vec![b"\x1b[A".to_vec(), b"\x1bOA".to_vec()]);
        assert_eq!(k("left"), vec![b"\x1b[D".to_vec(), b"\x1bOD".to_vec()]);
        assert_eq!(k("C-Left"), vec![b"\x1b[1;5D".to_vec()]);
        assert_eq!(k("M-Up"), vec![b"\x1b[1;3A".to_vec(), b"\x1b\x1b[A".to_vec()]);
        assert_eq!(k("S-Tab"), vec![b"\x1b[Z".to_vec()]);
        assert_eq!(k("PageUp"), vec![b"\x1b[5~".to_vec()]);
        assert_eq!(k("F1"), vec![b"\x1bOP".to_vec()]);
        assert_eq!(k("F5"), vec![b"\x1b[15~".to_vec()]);
        assert!(key_bytes("Foo").is_err());
        assert!(key_bytes("C-é").is_err());
        assert!(key_bytes("").is_err());
    }

    #[test]
    fn lookup() {
        let keys = Keys::default();
        assert_eq!(keys.match_prefix(b"\x02x"), Some(1));
        assert_eq!(keys.match_prefix(b"x"), None);
        assert_eq!(keys.lookup(b"c"), Some((Action::NewTab, 1)));
        assert_eq!(keys.lookup(b"5rest"), Some((Action::SelectTab(5), 1)));
        assert_eq!(keys.lookup(b"\x1b[Aabc"), Some((Action::FocusUp, 3)));
        assert_eq!(keys.lookup(b"\x1bOB"), Some((Action::FocusDown, 3)));
        assert_eq!(keys.lookup(b"\""), Some((Action::SplitDown, 1)));
        assert_eq!(keys.lookup(b"\x02"), Some((Action::SendPrefix, 1)));
        assert_eq!(keys.lookup(b"\x1b"), None);
        assert_eq!(keys.lookup(b"y"), None);
        assert_eq!(keys.key_for(Action::Detach), Some("d"));

        // Longest match: Esc bound alone must not swallow an arrow.
        let c = Config::parse("[keys]\nprefix = \"C-a\"\n[keys.bindings]\nEsc = \"detach\"\nM-Up = \"zoom\"\nc = \"none\"", None)
            .unwrap();
        let k = &c.keys;
        assert_eq!(k.match_prefix(b"\x01"), Some(1));
        assert_eq!(k.lookup(b"\x01"), Some((Action::SendPrefix, 1)));
        assert_eq!(k.lookup(b"\x02"), None);
        assert_eq!(k.lookup(b"\x1b"), Some((Action::Detach, 1)));
        assert_eq!(k.lookup(b"\x1b[A"), Some((Action::FocusUp, 3)));
        assert_eq!(k.lookup(b"\x1b[1;3A"), Some((Action::Zoom, 6)));
        assert_eq!(k.lookup(b"\x1b\x1b[A"), Some((Action::Zoom, 4)));
        assert_eq!(k.lookup(b"c"), None);
    }

    #[test]
    fn action_names() {
        for (name, a) in ACTION_NAMES {
            assert_eq!(name.parse::<Action>().unwrap(), *a);
            assert_eq!(a.to_string(), *name);
        }
        assert_eq!("select-tab-3".parse::<Action>().unwrap(), Action::SelectTab(3));
        assert!("select-tab-0".parse::<Action>().is_err());
        assert!("select-tab-10".parse::<Action>().is_err());
    }

    #[test]
    fn theme_colours() {
        let p = |s: &str| s.parse::<ThemeColor>().unwrap();
        assert_eq!(p("green"), ThemeColor::Color(Color::Green));
        assert_eq!(p("light-blue"), ThemeColor::Color(Color::LightBlue));
        assert_eq!(p("#ff8000"), ThemeColor::Color(Color::Rgb(255, 128, 0)));
        assert_eq!(p("236"), ThemeColor::Color(Color::Indexed(236)));
        assert_eq!(p("profile"), ThemeColor::Profile);
        assert_eq!(p("default"), ThemeColor::Color(Color::Reset));
        assert!("#12345".parse::<ThemeColor>().is_err());
        assert!("chartreuse".parse::<ThemeColor>().is_err());
        assert_eq!(ThemeColor::Profile.to_color(Color::Red), Color::Red);

        let c = Config::parse("[theme]\nbase = \"nord\"\nborder = 240\naccent = \"#00ff00\"", None).unwrap();
        assert_eq!(c.theme.border, ThemeColor::Color(Color::Indexed(240)));
        assert_eq!(c.theme.accent, ThemeColor::Color(Color::Rgb(0, 255, 0)));
        assert_eq!(c.theme.status_bg, Theme::builtin("nord").unwrap().status_bg);
        let c = Config::parse("theme = \"light\"", None).unwrap();
        assert_eq!(c.theme, Theme::builtin("light").unwrap());
        assert!(Config::parse("[theme]\nborder = 300", None).is_err());
        assert!(Config::parse("theme = \"nope\"", None).is_err());
        assert!(Config::parse("theme = \"../x\"", None).is_err());
    }

    #[test]
    fn theme_file() {
        let dir = std::env::temp_dir().join(format!("oui-theme-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("mine.toml"), "base = \"light\"\naccent = \"red\"").unwrap();
        let c = Config::parse("[theme]\nbase = \"mine\"\nborder = \"blue\"", Some(&dir)).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(c.theme.accent, ThemeColor::Color(Color::Red));
        assert_eq!(c.theme.border, ThemeColor::Color(Color::Blue));
        assert_eq!(c.theme.panel_bg, Theme::builtin("light").unwrap().panel_bg);
    }

    #[test]
    fn status_segments() {
        let c = Config::parse("[status]\nleft = [\"profile\", { text = \"│\" }, \"tabs-count\"]", None).unwrap();
        assert_eq!(c.status.left, vec![Segment::Profile, Segment::Text("│".into()), Segment::TabsCount]);
        assert_eq!(c.status.right, Status::default().right);
        assert!(Config::parse("[status]\nleft = [\"weather\"]", None).is_err());
    }

    #[test]
    fn bad_config_errors() {
        let err = |s: &str| format!("{:#}", Config::parse(s, None).unwrap_err());
        let e = err("[general]\nscrollback = \"lots\"");
        assert!(e.contains("line 2"), "{e}");
        let e = err("[general]\nmouse = true\nmose = false");
        assert!(e.contains("line 3") && e.contains("mose"), "{e}");
        let e = err("[keys.bindings]\nq = \"explode\"");
        assert!(e.contains("explode") && e.contains("line"), "{e}");
        let e = err("[keys]\nprefix = \"C-é\"");
        assert!(e.contains("prefix"), "{e}");
        let e = err("[panel\nvisible = true");
        assert!(e.contains("line 1"), "{e}");
    }
}
