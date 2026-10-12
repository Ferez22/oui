//! Import an existing tmux config (`~/.tmux.conf`) as an oUI `config.toml`.
//!
//! Only a conservative subset is translated: the prefix, prefix-table key
//! bindings for commands oUI has an action for, `unbind`, and the `mouse`,
//! `history-limit` and `default-shell` options. Every other line is reported
//! in `skipped` (and listed in a comment in the output) — never dropped silently.
//! See docs/tmux-import.md.

use crate::config::{self, DEFAULT_BINDINGS};
use crate::profile;
use anyhow::{Context, Result, bail};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Result of translating a tmux config.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportResult {
    /// oUI config text (loads with `Config::parse`).
    pub toml: String,
    /// What was translated: `line N: <tmux line>  ->  <oUI setting>`.
    pub imported: Vec<String>,
    /// Lines not translated: `line N: <tmux line>  (<reason>)`.
    pub skipped: Vec<String>,
}

/// The user's tmux config, if there is one: `$XDG_CONFIG_HOME/tmux/tmux.conf`,
/// `~/.config/tmux/tmux.conf`, then `~/.tmux.conf` (tmux's own search order).
pub fn find_tmux_conf() -> Option<PathBuf> {
    let home = profile::real_home().ok()?;
    let mut candidates = Vec::new();
    if let Ok(x) = std::env::var("XDG_CONFIG_HOME")
        && !x.is_empty()
    {
        candidates.push(PathBuf::from(x).join("tmux/tmux.conf"));
    }
    candidates.push(home.join(".config/tmux/tmux.conf"));
    candidates.push(home.join(".tmux.conf"));
    candidates.into_iter().find(|p| p.is_file())
}

/// The tmux config to offer importing on first launch: one exists and
/// `~/.oui/config.toml` doesn't.
pub fn should_offer() -> Option<PathBuf> {
    if config::path().ok()?.exists() {
        return None;
    }
    find_tmux_conf()
}

/// Translate tmux config text (header names the source as "tmux.conf").
#[allow(dead_code)] // library entry point (first-launch prompt, tests)
pub fn import(text: &str) -> ImportResult {
    import_with(text, "tmux.conf", &today())
}

/// Read and translate a tmux config file.
pub fn import_file(path: &Path) -> Result<ImportResult> {
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    Ok(import_with(&text, &tilde(path), &today()))
}

/// `path` with the home directory shown as `~`.
fn tilde(path: &Path) -> String {
    match profile::real_home().ok().and_then(|h| path.strip_prefix(h).ok().map(Path::to_path_buf)) {
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

/// Write an import result to `~/.oui/config.toml`. Refuses to replace an
/// existing file unless `force`. Returns the path written.
pub fn write_config(result: &ImportResult, force: bool) -> Result<PathBuf> {
    config::Config::parse(&result.toml, None).context("imported config does not load (bug)")?;
    let path = config::path()?;
    if path.exists() && !force {
        bail!(
            "{} already exists; not overwriting it.\n\
             Merge by hand: run `oui config import-tmux` without --write and copy the [keys] section,\n\
             or pass --force to replace the file.",
            path.display()
        );
    }
    crate::profile::ensure_root()?;
    crate::profile::write_private(&path, result.toml.as_bytes())?;
    Ok(path)
}

// ---------------------------------------------------------------------------
// Translation

#[derive(Default)]
struct State {
    prefix: Option<String>,
    mouse: Option<bool>,
    scrollback: Option<usize>,
    shell: Option<String>,
    /// oUI key notation -> (action or "none", source line for the comment).
    bindings: BTreeMap<String, (String, String)>,
    /// tmux `base-index`, for `select-window -t N`.
    base_index: u32,
    imported: Vec<String>,
    skipped: Vec<String>,
}

/// Translate with an explicit source name and date (for tests).
fn import_with(text: &str, source: &str, date: &str) -> ImportResult {
    let lines = logical_lines(text);
    let mut st = State::default();
    // base-index changes what `select-window -t N` means wherever it is set.
    for (_, line) in &lines {
        if let Ok(toks) = tokenize(line)
            && let Some((name, value)) = simple_set(&toks)
            && name == "base-index"
            && let Ok(n) = value.parse()
        {
            st.base_index = n;
        }
    }
    for (n, line) in &lines {
        let shown = one_line(line);
        match translate(&mut st, line) {
            Ok(Some(what)) => st.imported.push(format!("line {n}: {shown}  ->  {what}")),
            Ok(None) => {}
            Err(why) => st.skipped.push(format!("line {n}: {shown}  ({why})")),
        }
    }
    let toml = render(&st, source, date);
    ImportResult { toml, imported: st.imported, skipped: st.skipped }
}

/// Join `\` continuations and `{ … }` blocks; drop blank lines and comments.
/// Returns (first line number, text).
fn logical_lines(text: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut cur: Option<(usize, String)> = None;
    let mut depth = 0usize;
    for (i, raw) in text.lines().enumerate() {
        let n = i + 1;
        let line = raw.trim_end_matches('\r');
        let (num, mut acc) = cur.take().unwrap_or((n, String::new()));
        if acc.is_empty() {
            let t = line.trim();
            if t.is_empty() || t.starts_with('#') {
                continue;
            }
        }
        if !acc.is_empty() {
            acc.push(if depth > 0 { '\n' } else { ' ' });
        }
        let trailing = line.len() - line.trim_end_matches('\\').len();
        if trailing % 2 == 1 {
            acc.push_str(line[..line.len() - 1].trim_end());
            cur = Some((num, acc));
            continue;
        }
        acc.push_str(line.trim());
        let t = line.trim();
        if t.ends_with('{') {
            depth += 1;
        } else if t == "}" || t.starts_with("} ") {
            depth = depth.saturating_sub(1);
        }
        if depth > 0 {
            cur = Some((num, acc));
        } else {
            out.push((num, acc));
        }
    }
    if let Some(c) = cur {
        out.push(c);
    }
    out
}

/// A config line shown in one line, safe inside a TOML comment.
fn one_line(s: &str) -> String {
    let joined = s.split('\n').map(str::trim).collect::<Vec<_>>().join(" ");
    joined.chars().map(|c| if c.is_control() && c != '\t' { '?' } else { c }).collect()
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Word(String),
    /// Unquoted `;` or `\;`: a command separator (or the `;` key after `bind`).
    Sep,
}

/// Split a line into words like tmux does: quotes, backslash escapes, `#` comments.
fn tokenize(line: &str) -> Result<Vec<Tok>, String> {
    let mut toks = Vec::new();
    let mut chars = line.chars().peekable();
    loop {
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        let Some(&c) = chars.peek() else { break };
        if c == '#' {
            break;
        }
        let (mut word, mut quoted, mut sep_only, mut trailing_sep) = (String::new(), false, false, false);
        while let Some(c) = chars.next_if(|c| !c.is_whitespace()) {
            let at_end = chars.peek().is_none_or(|c| c.is_whitespace());
            match c {
                '\'' => {
                    quoted = true;
                    loop {
                        match chars.next() {
                            Some('\'') => break,
                            Some(x) => word.push(x),
                            None => return Err("unterminated quote".into()),
                        }
                    }
                }
                '"' => {
                    quoted = true;
                    loop {
                        match chars.next() {
                            Some('"') => break,
                            Some('\\') => match chars.next() {
                                Some(x) => word.push(x),
                                None => return Err("unterminated quote".into()),
                            },
                            Some(x) => word.push(x),
                            None => return Err("unterminated quote".into()),
                        }
                    }
                }
                '\\' => match chars.next() {
                    Some(';') if word.is_empty() && !quoted && chars.peek().is_none_or(|c| c.is_whitespace()) => {
                        sep_only = true;
                    }
                    Some(x) => word.push(x),
                    None => word.push('\\'),
                },
                ';' if at_end => trailing_sep = true,
                _ => word.push(c),
            }
        }
        if !sep_only && (quoted || !word.is_empty()) {
            toks.push(Tok::Word(word));
        }
        if sep_only || trailing_sep {
            toks.push(Tok::Sep);
        }
    }
    Ok(toks)
}

fn words(toks: &[Tok]) -> Option<Vec<&str>> {
    toks.iter()
        .map(|t| match t {
            Tok::Word(w) => Some(w.as_str()),
            Tok::Sep => None,
        })
        .collect()
}

/// `set [-g|-s|-o|-q] <name> <value>` with nothing else on the line.
fn simple_set(toks: &[Tok]) -> Option<(String, String)> {
    let w = words(toks)?;
    let (cmd, rest) = w.split_first()?;
    if !matches!(*cmd, "set" | "set-option") {
        return None;
    }
    let mut args = rest.iter();
    let mut pos = Vec::new();
    for a in args.by_ref() {
        if let Some(f) = a.strip_prefix('-').filter(|f| !f.is_empty() && pos.is_empty()) {
            if !f.chars().all(|c| matches!(c, 'g' | 's' | 'o' | 'q')) {
                return None;
            }
        } else {
            pos.push(*a);
        }
    }
    match pos.as_slice() {
        [name, value] => Some((name.to_string(), value.to_string())),
        _ => None,
    }
}

/// Translate one logical line. `Ok(Some(desc))` imported, `Ok(None)` nothing to
/// report, `Err(reason)` skipped.
fn translate(st: &mut State, line: &str) -> Result<Option<String>, String> {
    if line.contains('\n') {
        return Err("multi-line { } block".into());
    }
    let toks = tokenize(line)?;
    let Some(Tok::Word(cmd)) = toks.first() else {
        return Err("not a command".into());
    };
    match cmd.as_str() {
        "set" | "set-option" => {
            if words(&toks).is_none() {
                return Err("several commands on one line".into());
            }
            set_option(st, &toks)
        }
        "setw" | "set-window-option" => Err("window option (no oUI equivalent)".into()),
        "bind" | "bind-key" => bind(st, &toks[1..], line),
        "unbind" | "unbind-key" => {
            if words(&toks).is_none() {
                return Err("several commands on one line".into());
            }
            unbind(st, &toks[1..], line)
        }
        "run" | "run-shell" => Err("runs a shell command / plugin manager".into()),
        "if" | "if-shell" => Err("conditional (if-shell)".into()),
        "source" | "source-file" => Err("sources another file (import it separately)".into()),
        c if c.starts_with('%') => Err("config directive".into()),
        _ => Err("command not supported".into()),
    }
}

fn set_option(st: &mut State, toks: &[Tok]) -> Result<Option<String>, String> {
    let w = words(toks).unwrap_or_default();
    if w.iter().any(|a| a.starts_with('@')) {
        return Err("plugin option".into());
    }
    let Some((name, value)) = simple_set(toks) else {
        return Err("option form not supported".into());
    };
    match name.as_str() {
        "prefix" => {
            let key = tmux_key(&value)?;
            st.prefix = Some(key.clone());
            Ok(Some(format!("keys.prefix = \"{key}\"")))
        }
        "mouse" => {
            let on = match value.as_str() {
                "on" | "yes" | "1" | "true" => true,
                "off" | "no" | "0" | "false" => false,
                _ => return Err(format!("unknown mouse value '{value}'")),
            };
            st.mouse = Some(on);
            Ok(Some(format!("general.mouse = {on}")))
        }
        "history-limit" => {
            let n: usize = value.parse().map_err(|_| format!("invalid number '{value}'"))?;
            st.scrollback = Some(n);
            Ok(Some(format!("general.scrollback = {n}")))
        }
        "default-shell" => {
            st.shell = Some(value.clone());
            Ok(Some(format!("general.shell = \"{value}\"")))
        }
        "base-index" => Ok(Some(format!("used to number tabs for select-window (base-index {})", st.base_index))),
        "prefix2" => Err("second prefix not supported".into()),
        n if n.starts_with("status") || n.contains("style") || n.contains("format") || n.contains("colour") => {
            Err("status/styling (use oUI [theme] / [status])".into())
        }
        _ => Err("option has no oUI equivalent".into()),
    }
}

/// Is `key` (oUI notation) one of the default bindings? Compared by bytes, so
/// "Escape" matches "Esc".
fn default_binding(key: &str) -> Option<&'static str> {
    let seq = config::key_bytes(key).ok()?.into_iter().next()?;
    DEFAULT_BINDINGS
        .iter()
        .find(|(k, _)| config::key_bytes(k).ok().and_then(|v| v.into_iter().next()).as_ref() == Some(&seq))
        .map(|(k, _)| *k)
}

fn unbind(st: &mut State, args: &[Tok], line: &str) -> Result<Option<String>, String> {
    let w = words(args).unwrap_or_default();
    let mut all = false;
    let mut key = None;
    let mut it = w.iter();
    while let Some(a) = it.next() {
        match *a {
            "-n" => return Err("root-table key (no prefix) not supported".into()),
            "-T" => match it.next() {
                Some(&"prefix") => {}
                _ => return Err("key table not supported".into()),
            },
            "-a" => all = true,
            "-q" => {}
            f if f.starts_with('-') && f.len() > 1 && key.is_none() => {
                return Err(format!("unbind flag {f} not supported"));
            }
            k if key.is_none() => key = Some(k),
            _ => return Err("unexpected arguments".into()),
        }
    }
    if all {
        st.bindings.clear();
        for (k, _) in DEFAULT_BINDINGS {
            st.bindings.insert(k.to_string(), ("none".into(), "unbind -a".into()));
        }
        return Ok(Some("all default bindings removed".into()));
    }
    let key = tmux_key(key.ok_or("no key given")?)?;
    // Forget an earlier `bind` of the same key from this file.
    let earlier = st.bindings.remove(&key).is_some();
    if let Some(def) = default_binding(&key) {
        st.bindings.insert(def.to_string(), ("none".into(), one_line(line)));
        Ok(Some(format!("{} = \"none\"", toml_key(def))))
    } else if earlier {
        Ok(Some(format!("removed earlier binding of {key}")))
    } else {
        Ok(Some(format!("nothing to do ({key} is not bound by default in oUI)")))
    }
}

fn bind(st: &mut State, args: &[Tok], line: &str) -> Result<Option<String>, String> {
    let mut i = 0;
    let mut notes = Vec::new();
    // Flags before the key.
    while let Some(Tok::Word(a)) = args.get(i) {
        if !a.starts_with('-') || a.len() < 2 {
            break;
        }
        i += 1;
        let mut flags = a[1..].chars();
        while let Some(f) = flags.next() {
            match f {
                'r' => notes.push("-r repeat ignored"),
                'n' => return Err("root-table key (no prefix) not supported".into()),
                'T' => {
                    let table = if flags.as_str().is_empty() {
                        i += 1;
                        match args.get(i - 1) {
                            Some(Tok::Word(t)) => t.clone(),
                            _ => return Err("missing key table".into()),
                        }
                    } else {
                        flags.as_str().to_string()
                    };
                    if table != "prefix" {
                        return Err(format!("key table '{table}' not supported"));
                    }
                    break;
                }
                'N' => {
                    // Note text: separate argument unless attached.
                    if flags.as_str().is_empty() {
                        i += 1;
                    }
                    break;
                }
                _ => return Err(format!("bind flag -{f} not supported")),
            }
        }
    }
    let key = match args.get(i) {
        Some(Tok::Word(k)) => tmux_key(k)?,
        Some(Tok::Sep) => ";".to_string(),
        None => return Err("no key given".into()),
    };
    let cmd = words(&args[i + 1..]).ok_or("several commands bound to one key")?;
    if cmd.first() == Some(&"{") {
        return Err("command block".into());
    }
    let action = command_action(&cmd, st.base_index)?;
    st.bindings.insert(key.clone(), (action.clone(), one_line(line)));
    let mut desc = format!("{} = \"{action}\"", toml_key(&key));
    if !notes.is_empty() {
        let _ = write!(desc, " ({})", notes.join(", "));
    }
    Ok(Some(desc))
}

/// The oUI action for a bound tmux command.
fn command_action(cmd: &[&str], base_index: u32) -> Result<String, String> {
    let Some((name, args)) = cmd.split_first() else {
        return Err("no command".into());
    };
    let unsupported = || Err(format!("'{}' has no oUI equivalent", cmd.join(" ")));
    let a = |s: &str| s.to_string();
    match *name {
        "split-window" | "splitw" => {
            let mut right = false;
            let mut it = args.iter();
            while let Some(x) = it.next() {
                match *x {
                    "-h" => right = true,
                    "-v" => right = false,
                    "-c" => {
                        // Directory: oUI splits start in the client's directory.
                        it.next();
                    }
                    _ => return unsupported(),
                }
            }
            Ok(a(if right { "split-right" } else { "split-down" }))
        }
        "select-pane" | "selectp" => match args {
            [d] => direction(d, "focus").map_or_else(unsupported, Ok),
            _ => unsupported(),
        },
        "resize-pane" | "resizep" => match args {
            ["-Z"] => Ok(a("zoom")),
            [d] | [d, _] if args.get(1).is_none_or(|n| n.parse::<u32>().is_ok()) => {
                direction(d, "resize").map_or_else(unsupported, Ok)
            }
            _ => unsupported(),
        },
        "new-window" | "neww" => match args {
            [] | ["-c", _] => Ok(a("new-tab")),
            _ => unsupported(),
        },
        "next-window" | "next" if args.is_empty() => Ok(a("next-tab")),
        "previous-window" | "prev" if args.is_empty() => Ok(a("prev-tab")),
        "select-window" | "selectw" => match args {
            ["-t", t] => {
                let t = t.trim_start_matches(':').trim_start_matches('=');
                match t.parse::<u32>() {
                    Ok(n) if n >= base_index && n - base_index < 9 => Ok(format!("select-tab-{}", n - base_index + 1)),
                    _ => unsupported(),
                }
            }
            _ => unsupported(),
        },
        "kill-pane" | "killp" if args.is_empty() => Ok(a("close-pane")),
        "kill-window" | "killw" if args.is_empty() => Ok(a("close-tab")),
        "confirm-before" | "confirm" => {
            // confirm-before [-p prompt] <command>: the command string is one argument.
            let inner = match args {
                ["-p", _, c] => c,
                [c] => c,
                _ => return unsupported(),
            };
            let inner: Vec<String> = match tokenize(inner) {
                Ok(t) => match words(&t) {
                    Some(w) => w.into_iter().map(String::from).collect(),
                    None => return unsupported(),
                },
                Err(_) => return unsupported(),
            };
            let inner: Vec<&str> = inner.iter().map(String::as_str).collect();
            match inner.as_slice() {
                ["kill-pane" | "killp"] | ["kill-window" | "killw"] => command_action(&inner, base_index),
                _ => unsupported(),
            }
        }
        "detach" | "detach-client" if args.is_empty() => Ok(a("detach")),
        "copy-mode" if args.is_empty() => Ok(a("copy-mode")),
        "choose-session" | "choose-tree" if args.iter().all(|x| matches!(*x, "-s" | "-Z" | "-sZ" | "-Zs")) => {
            Ok(a("session-picker"))
        }
        "command-prompt" if args.last().is_some_and(|c| c.trim_start().starts_with("rename-window")) => {
            Ok(a("rename-tab"))
        }
        "send-prefix" if args.is_empty() => Ok(a("send-prefix")),
        _ => unsupported(),
    }
}

fn direction(flag: &str, verb: &str) -> Option<String> {
    let d = match flag {
        "-L" => "left",
        "-R" => "right",
        "-U" => "up",
        "-D" => "down",
        _ => return None,
    };
    Some(format!("{verb}-{d}"))
}

/// tmux key name -> oUI notation, validated.
fn tmux_key(k: &str) -> Result<String, String> {
    let bad = || format!("key '{k}' not supported");
    let k2 = match (k.strip_prefix('^'), k.strip_prefix('\\')) {
        // ^a is tmux shorthand for C-a.
        (Some(rest), _) if rest.chars().count() == 1 => format!("C-{}", rest.to_ascii_lowercase()),
        // A quoted '\;' is the character itself.
        (_, Some(rest)) if rest.chars().count() == 1 => rest.to_string(),
        _ => k.to_string(),
    };
    let mut rest = k2.as_str();
    let mut mods = String::new();
    while rest.len() > 2 {
        match rest.get(..2) {
            Some(m @ ("C-" | "M-" | "S-")) => mods.push_str(m),
            Some("c-") => mods.push_str("C-"),
            Some("m-") => mods.push_str("M-"),
            Some("s-") => mods.push_str("S-"),
            _ => break,
        }
        rest = &rest[2..];
    }
    let name = if rest.chars().count() == 1 {
        // A control letter is the same key in either case.
        if mods.contains("C-") { rest.to_ascii_lowercase() } else { rest.to_string() }
    } else {
        let canon = match rest.to_ascii_lowercase().as_str() {
            "enter" => "Enter",
            "escape" | "esc" => "Esc",
            "space" => "Space",
            "tab" => "Tab",
            "btab" => "BTab",
            "bspace" => "BSpace",
            "up" => "Up",
            "down" => "Down",
            "left" => "Left",
            "right" => "Right",
            "home" => "Home",
            "end" => "End",
            "ppage" | "pgup" | "pageup" => "PageUp",
            "npage" | "pgdn" | "pagedown" => "PageDown",
            "ic" | "insert" => "Insert",
            "dc" | "delete" => "Delete",
            f if f.len() <= 3 && f.starts_with('f') && f[1..].parse::<u8>().is_ok_and(|n| (1..=12).contains(&n)) => {
                return check(format!("{mods}F{}", &f[1..]), k);
            }
            _ => return Err(bad()),
        };
        canon.to_string()
    };
    check(format!("{mods}{name}"), k)
}

fn check(notation: String, original: &str) -> Result<String, String> {
    config::key_bytes(&notation).map_err(|_| format!("key '{original}' not supported"))?;
    Ok(notation)
}

// ---------------------------------------------------------------------------
// Output

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

fn render(st: &State, source: &str, date: &str) -> String {
    let mut s = String::new();
    let w = &mut s;
    let _ = writeln!(w, "# Imported from {} on {date} by `oui config import-tmux`.", one_line(source));
    let _ = writeln!(w, "# Anything not set here uses the oUI default (`oui config default` shows all).");
    if st.skipped.is_empty() {
        let _ = writeln!(w, "# Every tmux line was translated.");
    } else {
        let _ = writeln!(w, "#");
        let _ = writeln!(w, "# Not imported ({} lines, no oUI equivalent or not understood):", st.skipped.len());
        for l in &st.skipped {
            let _ = writeln!(w, "#   {l}");
        }
    }
    if st.mouse.is_some() || st.scrollback.is_some() || st.shell.is_some() {
        let _ = writeln!(w, "\n[general]");
        if let Some(sh) = &st.shell {
            let _ = writeln!(w, "shell = {}", toml_str(sh));
        }
        if let Some(n) = st.scrollback {
            let _ = writeln!(w, "scrollback = {n}");
        }
        if let Some(m) = st.mouse {
            let _ = writeln!(w, "mouse = {m}");
        }
    }
    let _ = writeln!(w, "\n[keys]");
    let _ = writeln!(w, "prefix = {}", toml_str(st.prefix.as_deref().unwrap_or("C-b")));
    if !st.bindings.is_empty() {
        let _ = writeln!(w, "\n# Layered over the oUI defaults; \"none\" removes a default binding.");
        let _ = writeln!(w, "[keys.bindings]");
        for (k, (action, src)) in &st.bindings {
            let _ = writeln!(w, "{} = {}  # {src}", toml_key(k), toml_str(action));
        }
    }
    s
}

/// Today's date (UTC) as YYYY-MM-DD.
fn today() -> String {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let days = (secs / 86_400) as i64;
    // Civil-from-days (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Action, Config};

    fn load(r: &ImportResult) -> Config {
        Config::parse(&r.toml, None).unwrap_or_else(|e| panic!("{e:#}\n{}", r.toml))
    }

    fn action(c: &Config, key: &str) -> Option<Action> {
        let seq = config::key_bytes(key).unwrap().remove(0);
        c.keys.bound(&seq)
    }

    const POPULAR: &str = r##"
# remap prefix from 'C-b' to 'C-a'
unbind C-b
set-option -g prefix C-a
bind-key C-a send-prefix

# split panes using | and -
bind | split-window -h -c "#{pane_current_path}"
bind - split-window -v -c '#{pane_current_path}'
unbind '"'
unbind %

# reload config file
bind r source-file ~/.tmux.conf \; display "Reloaded!"

# vim-style pane movement
bind h select-pane -L
bind j select-pane -D
bind k select-pane -U
bind l select-pane -R
bind -r H resize-pane -L 5
bind -r J resize-pane -D 5
bind -r K resize-pane -U 5
bind -r L resize-pane -R 5
bind -n M-Left select-pane -L

set -g mouse on
set -g history-limit 50000
set -g default-shell /bin/zsh
set -g base-index 1
setw -g mode-keys vi
bind-key -T copy-mode-vi v send-keys -X begin-selection

# status bar
set -g status-position top
set -g status-style 'bg=#333333 fg=#5eacd3'
set -g @plugin 'tmux-plugins/tpm'
set -g @plugin 'tmux-plugins/tmux-sensible'
run '~/.tmux/plugins/tpm/tpm'
"##;

    #[test]
    fn popular_config() {
        let r = import_with(POPULAR, "~/.tmux.conf", "2026-01-02");
        let c = load(&r);
        assert!(r.toml.starts_with("# Imported from ~/.tmux.conf on 2026-01-02"));
        assert_eq!(c.keys.prefix.notation, "C-a");
        assert_eq!(action(&c, "C-a"), Some(Action::SendPrefix));
        assert_eq!(action(&c, "|"), Some(Action::SplitRight));
        assert_eq!(action(&c, "-"), Some(Action::SplitDown));
        assert_eq!(action(&c, "\""), None);
        assert_eq!(action(&c, "%"), None);
        assert_eq!(action(&c, "h"), Some(Action::FocusLeft));
        assert_eq!(action(&c, "L"), Some(Action::ResizeRight));
        assert_eq!(action(&c, "r"), None);
        assert!(c.general.mouse);
        assert_eq!(c.general.scrollback, 50_000);
        assert_eq!(c.general.shell.as_deref(), Some("/bin/zsh"));
        // Every non-comment line is accounted for.
        for needle in ["source-file", "M-Left", "mode-keys", "copy-mode-vi", "status-position", "status-style", "@plugin", "run "] {
            assert!(r.skipped.iter().any(|s| s.contains(needle)), "{needle} not skipped: {:#?}", r.skipped);
            assert!(r.toml.contains(needle), "{needle} not listed in toml");
        }
        assert!(r.imported.iter().any(|s| s.contains("unbind C-b")));
        assert_eq!(r.imported.len() + r.skipped.len(), 28);
    }

    #[test]
    fn select_window_uses_base_index() {
        let src = "bind a select-window -t 0\nbind b select-window -t :1\n";
        let c = load(&import(src));
        assert_eq!(action(&c, "a"), Some(Action::SelectTab(1)));
        assert_eq!(action(&c, "b"), Some(Action::SelectTab(2)));
        let c = load(&import(&format!("{src}set -g base-index 1\n")));
        assert_eq!(action(&c, "a"), None);
        assert_eq!(action(&c, "b"), Some(Action::SelectTab(1)));
    }

    #[test]
    fn commands_and_keys() {
        let src = r##"
set -g prefix ^A
bind-key Escape copy-mode
bind Space next-window
bind BSpace previous-window
bind '\;' new-window
bind \; kill-pane
bind X confirm-before -p "kill-window #W? (y/n)" kill-window
bind q detach-client
bind S choose-session
bind w choose-tree -Z
bind , command-prompt -I "#W" "rename-window '%%'"
bind z resize-pane -Z
bind Enter new-window -c "#{pane_current_path}"
bind -T prefix M-x kill-pane
bind C-h select-pane -L
bind e split-window -h htop
bind o display-popup -E "lazygit"
set -g prefix2 C-s
"##;
        let r = import(src);
        let c = load(&r);
        assert_eq!(c.keys.prefix.notation, "C-a");
        assert_eq!(action(&c, "Esc"), Some(Action::CopyMode));
        assert_eq!(action(&c, "Space"), Some(Action::NextTab));
        assert_eq!(action(&c, "BSpace"), Some(Action::PrevTab));
        assert_eq!(action(&c, ";"), Some(Action::ClosePane));
        assert_eq!(action(&c, "X"), Some(Action::CloseTab));
        assert_eq!(action(&c, "q"), Some(Action::Detach));
        assert_eq!(action(&c, "S"), Some(Action::SessionPicker));
        assert_eq!(action(&c, "w"), Some(Action::SessionPicker));
        assert_eq!(action(&c, ","), Some(Action::RenameTab));
        assert_eq!(action(&c, "z"), Some(Action::Zoom));
        assert_eq!(action(&c, "Enter"), Some(Action::NewTab));
        assert_eq!(action(&c, "M-x"), Some(Action::ClosePane));
        assert_eq!(action(&c, "C-h"), Some(Action::FocusLeft));
        assert_eq!(action(&c, "e"), None);
        assert_eq!(r.skipped.len(), 3, "{:#?}", r.skipped);
    }

    #[test]
    fn continuations_blocks_and_separators() {
        let src = "bind -r h \\\n  select-pane -L\nbind x {\n  kill-pane\n}\nset -g mouse off ; set -g history-limit 5\n%if #{TMUX}\nif-shell 'true' 'set -g mouse on'\n";
        let r = import(src);
        let c = load(&r);
        assert_eq!(action(&c, "h"), Some(Action::FocusLeft));
        assert!(c.general.mouse, "multi-command line must be skipped");
        assert_eq!(r.imported.len(), 1, "{:#?}", r.imported);
        assert_eq!(r.skipped.len(), 4, "{:#?}", r.skipped);
        assert!(r.skipped[0].starts_with("line 3: bind x {"));
    }

    #[test]
    fn unbind_all_and_rebind() {
        let r = import("unbind -a\nbind c new-window\n");
        let c = load(&r);
        assert_eq!(action(&c, "c"), Some(Action::NewTab));
        assert_eq!(action(&c, "n"), None);
        assert_eq!(action(&c, "C-b"), Some(Action::SendPrefix));
    }

    #[test]
    fn bind_then_unbind_and_escape_name() {
        let r = import("bind v split-window -h\nunbind v\nunbind Escape\n");
        let c = load(&r);
        assert_eq!(action(&c, "v"), None);
        assert!(r.skipped.is_empty());
        assert!(r.toml.contains("Every tmux line was translated"));
    }

    #[test]
    fn real_world_minimal() {
        // Shape of a typical plugin-heavy config.
        let src = "unbind r\nbind-key r source-file ~/.tmux.conf \\; display-message \"reloaded\"\nset -g prefix C-s\nset -g mouse on\nbind-key h select-pane -L\nset -g @plugin 'catppuccin/tmux'\nset -g @catppuccin_window_left_separator \"\"\nrun '~/.tmux/plugins/tpm/tpm'\n";
        let r = import(src);
        let c = load(&r);
        assert_eq!(c.keys.prefix.notation, "C-s");
        assert_eq!(action(&c, "C-s"), Some(Action::SendPrefix));
        assert_eq!(action(&c, "h"), Some(Action::FocusLeft));
        assert_eq!(r.imported.len(), 4, "{:#?}", r.imported);
        assert_eq!(r.skipped.len(), 4, "{:#?}", r.skipped);
    }

    #[test]
    fn empty_and_garbage() {
        load(&import(""));
        let r = import("bind 'unterminated\nfoo bar\n\u{1}weird\n");
        load(&r);
        assert_eq!(r.skipped.len(), 3);
    }

    #[test]
    fn date_format() {
        let d = today();
        assert_eq!(d.len(), 10);
        assert_eq!(&d[4..5], "-");
    }
}
