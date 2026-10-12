//! Workspaces: TOML files declaring a session — its tabs, each tab's profile
//! and directory, and a tree of split panes with optional startup commands.
//!
//! Files live in `~/.oui/workspaces/<name>.toml` (or anywhere, by path). The
//! client parses and validates a file into a `WorkspaceSpec` (absolute dirs,
//! profiles checked) and sends it to the daemon, so mistakes are reported in
//! the terminal with file and line before anything is created.
//!
//! ```toml
//! name = "api"
//! cwd = "~/code/api"
//! [[tab]]
//! profile = "work"
//! layout = { split = "right", panes = [
//!   { cmd = "claude" },
//!   { split = "down", panes = [{ cmd = "railway logs" }, { cmd = "npm run dev" }] },
//! ] }
//! ```

use crate::client;
use crate::layout::{Axis, Tree};
use crate::profile;
use crate::protocol::{ClientMsg, ServerMsg, read_msg, write_msg};
use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use toml::Spanned;

/// A resolved workspace, as sent to the daemon (and returned by it for `save`).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct WorkspaceSpec {
    /// Session name.
    pub name: String,
    pub tabs: Vec<TabSpec>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct TabSpec {
    /// `None` = system (no isolation).
    pub profile: Option<String>,
    pub layout: Tree<PaneSpec>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct PaneSpec {
    /// Absolute directory; empty = unknown (daemon falls back to its own cwd).
    pub cwd: String,
    /// Typed into the pane's shell once it starts.
    pub cmd: Option<String>,
}

// ---- file format ----------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileSpec {
    name: Option<String>,
    cwd: Option<Spanned<String>>,
    #[serde(default)]
    tab: Vec<FileTab>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileTab {
    profile: Option<Spanned<String>>,
    cwd: Option<Spanned<String>>,
    layout: Option<FilePane>,
}

#[derive(Deserialize, Clone, Copy, PartialEq, Debug)]
#[serde(rename_all = "lowercase")]
enum SplitDir {
    Right,
    Down,
}

/// A layout node: a pane (`cmd`, `cwd`) or a split (`split`, `ratio`, `panes`).
#[derive(Deserialize)]
#[serde(try_from = "RawPane")]
enum FilePane {
    Shell { cmd: Option<String>, cwd: Option<Spanned<String>> },
    Split { dir: SplitDir, ratio: Option<f32>, panes: Vec<FilePane> },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPane {
    cmd: Option<String>,
    cwd: Option<Spanned<String>>,
    split: Option<SplitDir>,
    ratio: Option<f32>,
    panes: Option<Vec<FilePane>>,
}

impl TryFrom<RawPane> for FilePane {
    type Error = String;

    fn try_from(r: RawPane) -> Result<Self, String> {
        match (r.split, r.panes) {
            (None, None) => {
                if r.ratio.is_some() {
                    return Err("`ratio` only applies to a split (with `split` and `panes`)".into());
                }
                Ok(FilePane::Shell { cmd: r.cmd, cwd: r.cwd })
            }
            (Some(_), None) => Err("a split needs `panes = [...]`".into()),
            (None, Some(_)) => Err("`panes` needs `split = \"right\"` or `split = \"down\"`".into()),
            (Some(dir), Some(panes)) => {
                if r.cmd.is_some() || r.cwd.is_some() {
                    return Err("a split can't have `cmd` or `cwd`; put them on its panes".into());
                }
                if panes.len() < 2 {
                    return Err("a split needs at least 2 panes".into());
                }
                if let Some(x) = r.ratio
                    && !(0.1..=0.9).contains(&x)
                {
                    return Err(format!("ratio must be between 0.1 and 0.9 (got {x})"));
                }
                Ok(FilePane::Split { dir, ratio: r.ratio, panes })
            }
        }
    }
}

/// What `parse` needs from the outside world (swappable in tests).
pub struct Checks<'a> {
    pub profile: &'a dyn Fn(&str) -> Result<()>,
    pub dir: &'a dyn Fn(&Path) -> bool,
}

impl Checks<'static> {
    /// Real checks: profiles must exist under `~/.oui`, dirs on disk.
    pub fn real() -> Self {
        Checks { profile: &|p| profile::load(p).map(|_| ()), dir: &|d| d.is_dir() }
    }
}

fn line_of(src: &str, offset: usize) -> usize {
    src[..offset.min(src.len())].matches('\n').count() + 1
}

/// `~` and `~/x` use the real HOME; relative paths are joined to `base`.
fn expand(raw: &str, base: &Path) -> Result<PathBuf> {
    let p = if raw == "~" {
        profile::real_home()?
    } else if let Some(rest) = raw.strip_prefix("~/") {
        profile::real_home()?.join(rest)
    } else {
        base.join(raw)
    };
    Ok(p)
}

/// Parse workspace `src` (read from `path`); relative dirs resolve against `cwd`.
pub fn parse(src: &str, path: &Path, cwd: &Path, checks: &Checks) -> Result<WorkspaceSpec> {
    let file: FileSpec = toml::from_str(src).map_err(|e| anyhow!("{}: {e}", path.display()))?;
    let at = |span: std::ops::Range<usize>| format!("{}:{}", path.display(), line_of(src, span.start));
    let resolve = |raw: &Spanned<String>, base: &Path| -> Result<PathBuf> {
        let dir = expand(raw.get_ref(), base)?;
        if !(checks.dir)(&dir) {
            bail!("{}: directory not found: {}", at(raw.span()), dir.display());
        }
        Ok(dir)
    };
    let name = match file.name {
        Some(n) => n,
        None => path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
    };
    if name.trim().is_empty() {
        bail!("{}: workspace needs a `name`", path.display());
    }
    if file.tab.is_empty() {
        bail!("{}: workspace has no tabs (add a [[tab]] section)", path.display());
    }
    let base = match &file.cwd {
        Some(c) => resolve(c, cwd)?,
        None => cwd.to_path_buf(),
    };
    let mut tabs = Vec::new();
    for t in &file.tab {
        let profile = match &t.profile {
            Some(p) if p.get_ref() != profile::SYSTEM => {
                (checks.profile)(p.get_ref()).map_err(|e| anyhow!("{}: {e}", at(p.span())))?;
                Some(p.get_ref().clone())
            }
            _ => None,
        };
        let tab_cwd = match &t.cwd {
            Some(c) => resolve(c, &base)?,
            None => base.clone(),
        };
        let layout = match &t.layout {
            Some(l) => to_tree(l, &tab_cwd, &resolve)?,
            None => Tree::Leaf(PaneSpec { cwd: tab_cwd.to_string_lossy().into_owned(), cmd: None }),
        };
        tabs.push(TabSpec { profile, layout });
    }
    Ok(WorkspaceSpec { name, tabs })
}

/// File node → binary tree. A split of n panes gives the first `ratio` (default
/// an equal share) and shares the rest equally.
fn to_tree(
    p: &FilePane,
    cwd: &Path,
    resolve: &dyn Fn(&Spanned<String>, &Path) -> Result<PathBuf>,
) -> Result<Tree<PaneSpec>> {
    match p {
        FilePane::Shell { cmd, cwd: pane_cwd } => {
            let dir = match pane_cwd {
                Some(c) => resolve(c, cwd)?,
                None => cwd.to_path_buf(),
            };
            let cmd = cmd.clone().filter(|c| !c.trim().is_empty());
            Ok(Tree::Leaf(PaneSpec { cwd: dir.to_string_lossy().into_owned(), cmd }))
        }
        FilePane::Split { dir, ratio, panes } => {
            let axis = match dir {
                SplitDir::Right => Axis::Horizontal,
                SplitDir::Down => Axis::Vertical,
            };
            let mut nodes = panes.iter().map(|p| to_tree(p, cwd, resolve)).collect::<Result<Vec<_>>>()?;
            let n = nodes.len();
            let mut acc = nodes.pop().expect("split has >= 2 panes");
            for (i, node) in nodes.into_iter().enumerate().rev() {
                let r = match (i, ratio) {
                    (0, Some(r)) => *r,
                    _ => 1.0 / (n - i) as f32,
                };
                acc = Tree::Split { axis, ratio: r, a: Box::new(node), b: Box::new(acc) };
            }
            Ok(acc)
        }
    }
}

// ---- writing (save) -------------------------------------------------------

fn quote(s: &str) -> String {
    toml::Value::String(s.into()).to_string()
}

/// `~/x` for paths inside the real HOME.
fn tilde(path: &str) -> String {
    if let Ok(home) = profile::real_home()
        && let Ok(rest) = Path::new(path).strip_prefix(&home)
    {
        return if rest.as_os_str().is_empty() { "~".into() } else { format!("~/{}", rest.display()) };
    }
    path.into()
}

/// Render `spec` as a workspace file. Startup commands are written if present.
pub fn render(spec: &WorkspaceSpec) -> String {
    let mut out = format!(
        "# oUI workspace — open with: oui open {}\n\
         # Saved from a running session: startup commands aren't captured; add\n\
         # `cmd = \"...\"` to a pane (`{{}}` is a plain shell). Format: docs/workspaces.md\n\
         name = {}\n",
        spec.name,
        quote(&spec.name)
    );
    for tab in &spec.tabs {
        out.push_str("\n[[tab]]\n");
        let _ = writeln!(out, "profile = {}", quote(tab.profile.as_deref().unwrap_or(profile::SYSTEM)));
        let tab_cwd = tab.layout.leaves().iter().map(|p| p.cwd.clone()).find(|c| !c.is_empty()).unwrap_or_default();
        if !tab_cwd.is_empty() {
            let _ = writeln!(out, "cwd = {}", quote(&tilde(&tab_cwd)));
        }
        match &tab.layout {
            Tree::Leaf(p) if p.cmd.is_none() && (p.cwd.is_empty() || p.cwd == tab_cwd) => {}
            layout => {
                out.push_str("layout = ");
                render_node(layout, &tab_cwd, 0, &mut out);
                out.push('\n');
            }
        }
    }
    out
}

fn render_node(node: &Tree<PaneSpec>, tab_cwd: &str, depth: usize, out: &mut String) {
    match node {
        Tree::Leaf(p) => {
            let mut fields = Vec::new();
            if let Some(cmd) = &p.cmd {
                fields.push(format!("cmd = {}", quote(cmd)));
            }
            if !p.cwd.is_empty() && p.cwd != tab_cwd {
                fields.push(format!("cwd = {}", quote(&tilde(&p.cwd))));
            }
            if fields.is_empty() {
                out.push_str("{}");
            } else {
                let _ = write!(out, "{{ {} }}", fields.join(", "));
            }
        }
        Tree::Split { axis, ratio, a, b } => {
            let dir = if *axis == Axis::Horizontal { "right" } else { "down" };
            let _ = write!(out, "{{ split = \"{dir}\", ");
            let r = (ratio * 100.0).round() / 100.0;
            if (r - 0.5).abs() > f32::EPSILON {
                let _ = write!(out, "ratio = {r}, ");
            }
            out.push_str("panes = [\n");
            let pad = "  ".repeat(depth + 1);
            for child in [a, b] {
                out.push_str(&pad);
                render_node(child, tab_cwd, depth + 1, out);
                out.push_str(",\n");
            }
            let _ = write!(out, "{}] }}", "  ".repeat(depth));
        }
    }
}

// ---- files and commands ---------------------------------------------------

/// `~/.oui/workspaces`.
pub fn dir() -> Result<PathBuf> {
    Ok(profile::oui_root()?.join("workspaces"))
}

fn check_name(name: &str) -> Result<()> {
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)) || name.starts_with('.') {
        bail!("workspace name must be letters, digits, '-', '_' or '.'");
    }
    Ok(())
}

/// A path (contains `/` or ends in `.toml`) or a name in `~/.oui/workspaces`.
pub fn locate(arg: &str) -> Result<PathBuf> {
    if arg.contains('/') || arg.ends_with(".toml") {
        let p = PathBuf::from(arg);
        if !p.is_file() {
            bail!("workspace file not found: {arg}");
        }
        return Ok(p);
    }
    check_name(arg)?;
    let p = dir()?.join(format!("{arg}.toml"));
    if !p.is_file() {
        bail!("no workspace '{arg}' ({} not found; create it: oui workspace new {arg})", p.display());
    }
    Ok(p)
}

pub fn load(path: &Path) -> Result<WorkspaceSpec> {
    let src = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    parse(&src, path, &std::env::current_dir()?, &Checks::real())
}

/// `oui open`: parse, validate, then create-or-attach.
pub async fn open(arg: &str) -> Result<()> {
    let spec = load(&locate(arg)?)?;
    client::open_workspace(spec).await
}

pub fn list() -> Result<()> {
    let dir = dir()?;
    let mut files: Vec<PathBuf> = fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
        .collect();
    files.sort();
    if files.is_empty() {
        println!("no workspaces in {} (create one: oui workspace new <name>)", dir.display());
    }
    let any = Checks { profile: &|_| Ok(()), dir: &|_| true };
    for f in files {
        let stem = f.file_stem().unwrap_or_default().to_string_lossy().into_owned();
        let parsed = fs::read_to_string(&f).map_err(Into::into).and_then(|src| parse(&src, &f, Path::new("/"), &any));
        match parsed {
            Ok(spec) => {
                let profiles: Vec<&str> =
                    spec.tabs.iter().map(|t| t.profile.as_deref().unwrap_or(profile::SYSTEM)).collect();
                let panes: usize = spec.tabs.iter().map(|t| t.layout.leaves().len()).sum();
                println!("{stem:<16} {} tab(s), {panes} pane(s)  [{}]", spec.tabs.len(), profiles.join(", "));
            }
            Err(e) => println!("{stem:<16} invalid: {}", format!("{e:#}").lines().next().unwrap_or_default()),
        }
    }
    Ok(())
}

const TEMPLATE: &str = r#"# oUI workspace — open with: oui open {name}
# Format: docs/workspaces.md in the oUI repository.
#
# Session name (default: the file name). If a session with this name is
# already running, `oui open` attaches to it instead of creating another.
name = "{name}"

# Default directory for every tab. `~` is your real home; relative paths are
# relative to where you run `oui open`.
# cwd = "~/code/{name}"

# One [[tab]] per tab. Every pane of a tab runs in the tab's profile.
[[tab]]
profile = "system"        # or a profile from `oui profile list`
# cwd = "~/code/{name}"   # overrides the default above

# A pane is `{}` (plain shell) or `{ cmd = "..." }`: the command is typed into
# the pane's shell, so the shell stays when it exits. A split is
# `{ split = "right" | "down", ratio = 0.5, panes = [...] }`; splits nest.
layout = { split = "right", panes = [
  {},
  { split = "down", panes = [
    { cmd = "git status" },
    {},
  ] },
] }

# [[tab]]
# profile = "work"
# layout = { cmd = "claude" }
"#;

pub fn template(name: &str) -> String {
    TEMPLATE.replace("{name}", name)
}

pub fn new(name: &str) -> Result<()> {
    check_name(name)?;
    let dir = dir()?;
    profile::ensure_root()?;
    profile::private_dir(&dir)?;
    let path = dir.join(format!("{name}.toml"));
    if path.exists() {
        bail!("{} already exists", path.display());
    }
    profile::write_private(&path, template(name).as_bytes())?;
    println!("created {} — edit it, then: oui open {name}", path.display());
    Ok(())
}

/// Ask the daemon for the shape of a running session.
async fn describe(session: &str) -> Result<WorkspaceSpec> {
    let mut s = client::connect(false).await?;
    write_msg(&mut s, &ClientMsg::DescribeSession(session.into())).await?;
    match read_msg::<_, ServerMsg>(&mut s).await? {
        ServerMsg::Workspace(spec) => Ok(spec),
        ServerMsg::Error(e) => bail!(e),
        other => bail!("unexpected reply: {other:?}"),
    }
}

/// `oui workspace save`: write a running session's layout to a workspace file.
pub async fn save(session: &str, name: Option<&str>, force: bool) -> Result<()> {
    let name = name.unwrap_or(session);
    check_name(name)?;
    let mut spec = describe(session).await?;
    spec.name = name.into();
    let dir = dir()?;
    profile::ensure_root()?;
    profile::private_dir(&dir)?;
    let path = dir.join(format!("{name}.toml"));
    if path.exists() && !force {
        bail!("{} already exists (use --force to overwrite)", path.display());
    }
    profile::write_private(&path, render(&spec).as_bytes())?;
    println!("saved session '{session}' to {}", path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok_checks() -> Checks<'static> {
        Checks {
            profile: &|p| if p == "missing" { bail!("profile '{p}' not found") } else { Ok(()) },
            dir: &|d| !d.ends_with("nope"),
        }
    }

    fn parse_str(src: &str) -> Result<WorkspaceSpec> {
        parse(src, Path::new("/ws/api.toml"), Path::new("/start"), &ok_checks())
    }

    fn leaf(cwd: &str, cmd: Option<&str>) -> Tree<PaneSpec> {
        Tree::Leaf(PaneSpec { cwd: cwd.into(), cmd: cmd.map(Into::into) })
    }

    #[test]
    fn parses_nested_layout() {
        let spec = parse_str(
            r#"
name = "proj"
cwd = "/code"
[[tab]]
profile = "work"
cwd = "api"
layout = { split = "right", ratio = 0.6, panes = [
  { cmd = "claude" },
  { split = "down", panes = [
    { cmd = "railway logs" },
    { cmd = "npm run dev", cwd = "web" },
  ] },
] }
[[tab]]
profile = "system"
"#,
        )
        .unwrap();
        assert_eq!(spec.name, "proj");
        assert_eq!(spec.tabs.len(), 2);
        assert_eq!(spec.tabs[0].profile.as_deref(), Some("work"));
        assert_eq!(
            spec.tabs[0].layout,
            Tree::Split {
                axis: Axis::Horizontal,
                ratio: 0.6,
                a: Box::new(leaf("/code/api", Some("claude"))),
                b: Box::new(Tree::Split {
                    axis: Axis::Vertical,
                    ratio: 0.5,
                    a: Box::new(leaf("/code/api", Some("railway logs"))),
                    b: Box::new(leaf("/code/api/web", Some("npm run dev"))),
                }),
            }
        );
        assert_eq!(spec.tabs[1].profile, None);
        assert_eq!(spec.tabs[1].layout, leaf("/code", None));
    }

    #[test]
    fn name_defaults_to_file_stem_and_cwd_to_start_dir() {
        let spec = parse_str("[[tab]]\nlayout = { cmd = \"htop\" }\n").unwrap();
        assert_eq!(spec.name, "api");
        assert_eq!(spec.tabs[0].layout, leaf("/start", Some("htop")));
    }

    #[test]
    fn tilde_uses_real_home() {
        let home = profile::real_home().unwrap();
        let spec = parse_str("cwd = \"~\"\n[[tab]]\ncwd = \"~/x\"\n[[tab]]\n").unwrap();
        assert_eq!(spec.tabs[0].layout, leaf(&home.join("x").to_string_lossy(), None));
        assert_eq!(spec.tabs[1].layout, leaf(&home.to_string_lossy(), None));
    }

    #[test]
    fn three_way_split_shares_space_equally() {
        let spec = parse_str("[[tab]]\nlayout = { split = \"down\", panes = [{}, {}, {}] }\n").unwrap();
        let Tree::Split { ratio, b, .. } = &spec.tabs[0].layout else { panic!() };
        assert!((ratio - 1.0 / 3.0).abs() < 1e-6);
        assert!(matches!(**b, Tree::Split { ratio, .. } if ratio == 0.5));
        assert_eq!(spec.tabs[0].layout.leaves().len(), 3);
    }

    fn err(src: &str) -> String {
        format!("{:#}", parse_str(src).unwrap_err())
    }

    #[test]
    fn errors_name_file_and_line() {
        let e = err("name = \"x\"\n[[tab]]\nprofile = \"missing\"\n");
        assert!(e.starts_with("/ws/api.toml:3: profile 'missing' not found"), "{e}");
        let e = err("[[tab]]\n\ncwd = \"nope\"\n");
        assert!(e.starts_with("/ws/api.toml:3: directory not found: /start/nope"), "{e}");
        let e = err("[[tab]]\nlayout = { split = \"right\", panes = [\n  { cwd = \"nope\" },\n  {},\n] }\n");
        assert!(e.starts_with("/ws/api.toml:3: directory not found"), "{e}");
    }

    #[test]
    fn layout_mistakes_are_reported_with_location() {
        for (src, want) in [
            ("[[tab]]\nlayout = { split = \"left\", panes = [{}, {}] }\n", "unknown variant"),
            ("[[tab]]\nlayout = { split = \"right\" }\n", "needs `panes"),
            ("[[tab]]\nlayout = { panes = [{}, {}] }\n", "needs `split"),
            ("[[tab]]\nlayout = { split = \"down\", panes = [{}] }\n", "at least 2 panes"),
            ("[[tab]]\nlayout = { split = \"down\", ratio = 1.5, panes = [{}, {}] }\n", "between 0.1 and 0.9"),
            ("[[tab]]\nlayout = { split = \"down\", cmd = \"x\", panes = [{}, {}] }\n", "can't have `cmd`"),
            ("[[tab]]\nlayout = { command = \"x\" }\n", "unknown field"),
            ("[[tab]]\nprofil = \"x\"\n", "unknown field"),
        ] {
            let e = err(src);
            assert!(e.contains(want), "{src:?}: {e}");
            assert!(e.contains("line 2"), "{src:?}: {e}");
            assert!(e.starts_with("/ws/api.toml"), "{e}");
        }
        assert!(err("name = \"x\"\n").contains("no tabs"));
    }

    #[test]
    fn render_round_trips() {
        let spec = parse_str(
            r#"
[[tab]]
profile = "work"
cwd = "/code"
layout = { split = "right", ratio = 0.7, panes = [
  { cmd = "say \"hi\"" },
  { split = "down", panes = [{ cwd = "/tmp" }, {}] },
] }
[[tab]]
"#,
        )
        .unwrap();
        let text = render(&spec);
        let back = parse_str(&text).unwrap();
        assert_eq!(back, spec, "{text}");
        assert!(text.contains("ratio = 0.7"), "{text}");
    }

    #[test]
    fn template_and_example_parse() {
        let any = Checks { profile: &|_| Ok(()), dir: &|_| true };
        let t = parse(&template("demo"), Path::new("demo.toml"), Path::new("/"), &any).unwrap();
        assert_eq!(t.name, "demo");
        assert_eq!(t.tabs[0].layout.leaves().len(), 3);
        let example = include_str!("../docs/workspace.example.toml");
        let e = parse(example, Path::new("example.toml"), Path::new("/"), &any).unwrap();
        assert!(e.tabs.len() >= 2);
    }
}
