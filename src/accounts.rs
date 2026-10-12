//! Connected accounts: which CLI tools are logged in for a profile (or system).
//!
//! Detection is offline: it never runs tool commands (they may hit the
//! network or block on a prompt) and never reads keyring secrets. Secrets are
//! never returned: config files are parsed into structs that keep only
//! non-secret identity fields (email, username, account name); token fields are
//! checked for presence only. A login kept only in a keyring shows as
//! `Unknown`, except Claude Code in a profile's own macOS keychain, which is
//! looked up by item attributes only (see `keychain::has_service_prefix`).
//!
//! To support a new tool, add a checker below and a row to `TOOLS`.

use anyhow::Result;
use serde::Deserialize;
use serde::de::IgnoredAny;
use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use crate::profile;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoginState {
    LoggedIn,
    LoggedOut,
    /// Login may live in a keyring we deliberately don't read.
    Unknown,
}

impl LoginState {
    pub fn label(self) -> &'static str {
        match self {
            LoginState::LoggedIn => "logged in",
            LoginState::LoggedOut => "logged out",
            LoginState::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ToolStatus {
    pub tool: &'static str,
    pub installed: bool,
    pub state: LoginState,
    /// Non-secret identity (email, username, account or profile names).
    pub account: Option<String>,
    /// Command to run inside the profile to log in.
    pub login_cmd: &'static str,
}

/// One supported tool. `check` looks at files only; `env_tokens` are variables
/// whose presence (never value) also means "logged in".
pub struct Tool {
    pub name: &'static str,
    pub bins: &'static [&'static str],
    pub login_cmd: &'static str,
    pub env_tokens: &'static [&'static str],
    pub check: fn(&Env) -> Check,
}

/// Result of a tool's file check.
pub type Check = (LoginState, Option<String>);

pub const TOOLS: &[Tool] = &[
    Tool { name: "claude", bins: &["claude"], login_cmd: "claude /login",
           env_tokens: &["ANTHROPIC_API_KEY", "CLAUDE_CODE_OAUTH_TOKEN"], check: claude },
    Tool { name: "railway", bins: &["railway"], login_cmd: "railway login",
           env_tokens: &["RAILWAY_TOKEN", "RAILWAY_API_TOKEN"], check: railway },
    Tool { name: "gh", bins: &["gh"], login_cmd: "gh auth login",
           env_tokens: &["GH_TOKEN", "GITHUB_TOKEN"], check: gh },
    Tool { name: "vercel", bins: &["vercel"], login_cmd: "vercel login",
           env_tokens: &["VERCEL_TOKEN"], check: vercel },
    Tool { name: "git", bins: &["git"], login_cmd: "git config --global user.email you@example.com",
           env_tokens: &[], check: git },
    Tool { name: "npm", bins: &["npm"], login_cmd: "npm login",
           env_tokens: &[], check: npm },
    Tool { name: "aws", bins: &["aws"], login_cmd: "aws configure",
           env_tokens: &["AWS_ACCESS_KEY_ID"], check: aws },
    Tool { name: "stripe", bins: &["stripe"], login_cmd: "stripe login",
           env_tokens: &["STRIPE_API_KEY"], check: stripe },
    Tool { name: "fly", bins: &["fly", "flyctl"], login_cmd: "fly auth login",
           env_tokens: &["FLY_API_TOKEN", "FLY_ACCESS_TOKEN"], check: fly },
    Tool { name: "supabase", bins: &["supabase"], login_cmd: "supabase login",
           env_tokens: &["SUPABASE_ACCESS_TOKEN"], check: supabase },
    Tool { name: "docker", bins: &["docker"], login_cmd: "docker login",
           env_tokens: &[], check: docker },
];

/// The environment a profile's processes see: variable lookup plus platform.
pub struct Env {
    vars: HashMap<String, String>,
    pub macos: bool,
}

impl Env {
    pub fn new(vars: HashMap<String, String>) -> Self {
        Env { vars, macos: cfg!(target_os = "macos") }
    }

    /// Environment of a shell in `profile` (`None` = system), as `profile::env_for` builds it.
    pub fn for_profile(profile: Option<&str>) -> Result<Self> {
        let mut vars: HashMap<String, String> = std::env::vars().collect();
        let spawn = profile::env_for(profile)?;
        for k in &spawn.remove {
            vars.remove(k);
        }
        vars.extend(spawn.set);
        Ok(Env::new(vars))
    }

    /// A non-empty variable.
    pub fn var(&self, k: &str) -> Option<&str> {
        self.vars.get(k).map(String::as_str).filter(|v| !v.is_empty())
    }

    fn path_var(&self, k: &str) -> Option<PathBuf> {
        self.var(k).map(PathBuf::from)
    }

    fn home(&self) -> PathBuf {
        self.path_var("HOME").unwrap_or_default()
    }

    fn xdg(&self, var: &str, default: &str) -> PathBuf {
        self.path_var(var).unwrap_or_else(|| self.home().join(default))
    }

    fn on_path(&self, bin: &str) -> bool {
        let Some(path) = self.var("PATH") else { return false };
        path.split(':').filter(|d| !d.is_empty()).any(|d| {
            fs::metadata(Path::new(d).join(bin))
                .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
                .unwrap_or(false)
        })
    }
}

/// Login status of every known tool for `profile` (`None` or `"system"` = real environment).
pub fn detect(profile: Option<&str>) -> Result<Vec<ToolStatus>> {
    let profile = profile.filter(|p| *p != profile::SYSTEM);
    Ok(detect_env(&Env::for_profile(profile)?))
}

pub fn detect_env(env: &Env) -> Vec<ToolStatus> {
    TOOLS
        .iter()
        .map(|t| {
            let (mut state, mut account) = (t.check)(env);
            if state != LoginState::LoggedIn
                && let Some(var) = t.env_tokens.iter().find(|v| env.var(v).is_some())
            {
                state = LoginState::LoggedIn;
                account = Some(format!("${var}"));
            }
            ToolStatus {
                tool: t.name,
                installed: t.bins.iter().any(|b| env.on_path(b)),
                state,
                account,
                login_cmd: t.login_cmd,
            }
        })
        .collect()
}

/// `oui accounts [profile]`
pub fn print_table(profile: Option<&str>) -> Result<()> {
    let rows = detect(profile)?;
    println!("{:<10} {:<10} {:<11} {:<32} LOGIN", "TOOL", "INSTALLED", "STATE", "ACCOUNT");
    for r in rows {
        let installed = if r.installed { "yes" } else { "no" };
        let account = r.account.as_deref().unwrap_or("-");
        println!("{:<10} {:<10} {:<11} {:<32} {}", r.tool, installed, r.state.label(), account, r.login_cmd);
    }
    Ok(())
}

// ---- helpers ----------------------------------------------------------------

/// Config files are small; anything bigger is not what we expect.
const MAX_FILE: u64 = 32 << 20;

fn read(path: &Path) -> Option<String> {
    if fs::metadata(path).ok()?.len() > MAX_FILE {
        return None;
    }
    fs::read_to_string(path).ok()
}

fn json<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
    serde_json::from_str(&read(path)?).ok()
}

/// Lines of an INI/rc/YAML-ish file without comments and blank lines.
fn lines(text: &str) -> impl Iterator<Item = &str> {
    text.lines().filter(|l| {
        let t = l.trim_start();
        !t.is_empty() && !t.starts_with('#') && !t.starts_with(';')
    })
}

/// `key = value` / `key: value` split; value trimmed and unquoted.
fn kv(line: &str, sep: char) -> Option<(&str, &str)> {
    let (k, v) = line.split_once(sep)?;
    Some((k.trim(), unquote(v)))
}

fn unquote(v: &str) -> &str {
    let v = v.trim();
    let v = v.strip_prefix('"').and_then(|v| v.strip_suffix('"')).unwrap_or(v);
    v.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')).unwrap_or(v)
}

/// INI section header: `[name]` → `name`.
fn section(line: &str) -> Option<&str> {
    line.trim().strip_prefix('[')?.strip_suffix(']').map(str::trim)
}

fn join(items: Vec<String>) -> Option<String> {
    (!items.is_empty()).then(|| items.join(", "))
}

fn logged_out() -> Check {
    (LoginState::LoggedOut, None)
}

// ---- checkers ---------------------------------------------------------------

/// Claude Code: `oauthAccount.emailAddress` in `.claude.json` (non-secret) or
/// a `.credentials.json` file (Linux; never opened). On macOS the token is in
/// the Keychain: in a profile, an item whose service starts with "Claude Code"
/// in the profile keychain means logged in (attributes only; a locked keychain
/// is never unlocked). Otherwise a config without a marker is `Unknown`.
fn claude(env: &Env) -> Check {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Cfg {
        oauth_account: Option<Account>,
        primary_api_key: Option<IgnoredAny>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Account {
        email_address: Option<String>,
    }
    let (dir, cfg_path) = match env.path_var("CLAUDE_CONFIG_DIR") {
        Some(d) => (d.clone(), d.join(".claude.json")),
        None => (env.home().join(".claude"), env.home().join(".claude.json")),
    };
    let has_creds = dir.join(".credentials.json").is_file();
    let cfg: Option<Cfg> = json(&cfg_path);
    if let Some(Cfg { oauth_account: Some(a), .. }) = &cfg {
        return (LoginState::LoggedIn, a.email_address.clone());
    }
    if has_creds || matches!(&cfg, Some(Cfg { primary_api_key: Some(_), .. })) {
        return (LoginState::LoggedIn, None);
    }
    let in_profile = env.var("OUI_PROFILE").is_some_and(|p| p != profile::SYSTEM);
    if env.macos && in_profile && crate::keychain::has_service_prefix(&env.home(), "Claude Code") {
        return (LoginState::LoggedIn, None);
    }
    if cfg.is_some() && env.macos {
        return (LoginState::Unknown, None);
    }
    logged_out()
}

/// Railway: `~/.railway/config.json` with a `user.token`/`user.accessToken` (presence only).
fn railway(env: &Env) -> Check {
    #[derive(Deserialize)]
    struct Cfg {
        user: Option<User>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct User {
        token: Option<IgnoredAny>,
        access_token: Option<IgnoredAny>,
    }
    match json::<Cfg>(&env.home().join(".railway/config.json")) {
        Some(Cfg { user: Some(User { token, access_token }) }) if token.is_some() || access_token.is_some() => {
            (LoginState::LoggedIn, None)
        }
        _ => logged_out(),
    }
}

/// gh: `hosts.yml` per-host `user:` field. Tokens may sit next to it (or in a
/// keyring); only the `user` key is read.
fn gh(env: &Env) -> Check {
    let dir = env.path_var("GH_CONFIG_DIR").unwrap_or_else(|| env.xdg("XDG_CONFIG_HOME", ".config").join("gh"));
    let Some(text) = read(&dir.join("hosts.yml")) else { return logged_out() };
    let mut users = vec![];
    let (mut host, mut indent) = (None, None);
    for line in lines(&text) {
        let depth = line.len() - line.trim_start().len();
        if depth == 0 {
            host = line.trim_end().strip_suffix(':');
            indent = None;
            continue;
        }
        let Some(h) = host else { continue };
        if *indent.get_or_insert(depth) != depth {
            continue;
        }
        if let Some(("user", u)) = kv(line, ':')
            && !u.is_empty()
        {
            users.push(if h == "github.com" { u.to_string() } else { format!("{u}@{h}") });
        }
    }
    match join(users) {
        Some(a) => (LoginState::LoggedIn, Some(a)),
        None => logged_out(),
    }
}

/// Vercel: `auth.json` in the CLI's data dir with a `token` (presence only).
/// No identity is stored locally (`vercel whoami` needs the network).
fn vercel(env: &Env) -> Check {
    #[derive(Deserialize)]
    struct Auth {
        token: Option<IgnoredAny>,
    }
    let home = env.home();
    let mut dirs = vec![];
    if let Some(d) = env.path_var("XDG_DATA_HOME") {
        dirs.push(d.join("com.vercel.cli"));
    }
    if env.macos {
        dirs.push(home.join("Library/Application Support/com.vercel.cli"));
    }
    dirs.extend([home.join(".local/share/com.vercel.cli"), home.join(".now")]);
    for d in dirs {
        if let Some(a) = json::<Auth>(&d.join("auth.json")) {
            return if a.token.is_some() { (LoginState::LoggedIn, None) } else { logged_out() };
        }
    }
    logged_out()
}

/// git identity: `user.email` from the global config (`GIT_CONFIG_GLOBAL`, or
/// XDG `git/config` then `~/.gitconfig`). `[include]` files are not followed.
fn git(env: &Env) -> Check {
    let files = match env.path_var("GIT_CONFIG_GLOBAL") {
        Some(f) => vec![f],
        None => vec![env.xdg("XDG_CONFIG_HOME", ".config").join("git/config"), env.home().join(".gitconfig")],
    };
    let mut email = None;
    for text in files.iter().filter_map(|f| read(f)) {
        let mut in_user = false;
        for line in lines(&text) {
            if let Some(s) = section(line) {
                in_user = s.eq_ignore_ascii_case("user");
            } else if in_user
                && let Some((k, v)) = kv(line, '=')
                && k.eq_ignore_ascii_case("email")
            {
                let v = unquote(v.split(['#', ';']).next().unwrap_or(""));
                email = (!v.is_empty()).then(|| v.to_string());
            }
        }
    }
    match email {
        Some(e) => (LoginState::LoggedIn, Some(e)),
        None => logged_out(),
    }
}

/// npm: an `_authToken`/`_auth` key in the user `.npmrc` (keys only, values never read).
fn npm(env: &Env) -> Check {
    let path = env
        .path_var("NPM_CONFIG_USERCONFIG")
        .or_else(|| env.path_var("npm_config_userconfig"))
        .unwrap_or_else(|| env.home().join(".npmrc"));
    let Some(text) = read(&path) else { return logged_out() };
    let mut authed = false;
    let mut email = None;
    for line in lines(&text) {
        let Some((k, v)) = kv(line, '=') else { continue };
        let key = k.rsplit(':').next().unwrap_or(k);
        if matches!(key, "_authToken" | "_auth" | "_password") {
            authed = true;
        } else if k == "email" && !v.is_empty() {
            email = Some(v.to_string());
        }
    }
    if authed { (LoginState::LoggedIn, email) } else { logged_out() }
}

/// AWS: profiles with `aws_access_key_id` in the credentials file, or SSO
/// profiles with a cached SSO token (may be expired). Account = profile names.
fn aws(env: &Env) -> Check {
    let aws = env.home().join(".aws");
    let creds = env.path_var("AWS_SHARED_CREDENTIALS_FILE").unwrap_or_else(|| aws.join("credentials"));
    let config = env.path_var("AWS_CONFIG_FILE").unwrap_or_else(|| aws.join("config"));
    let profiles_with = |path: &Path, keys: &[&str]| -> Vec<String> {
        let Some(text) = read(path) else { return vec![] };
        let mut out: Vec<String> = vec![];
        let mut cur: Option<String> = None;
        for line in lines(&text) {
            if let Some(s) = section(line) {
                cur = Some(s.strip_prefix("profile ").unwrap_or(s).trim().to_string());
            } else if let (Some(p), Some((k, _))) = (&cur, kv(line, '='))
                && keys.contains(&k)
                && !out.contains(p)
            {
                out.push(p.clone());
            }
        }
        out
    };
    let mut names = profiles_with(&creds, &["aws_access_key_id"]);
    let sso_cached = fs::read_dir(aws.join("sso/cache"))
        .map(|d| d.flatten().any(|e| e.path().extension().is_some_and(|x| x == "json")))
        .unwrap_or(false);
    if sso_cached {
        for p in profiles_with(&config, &["sso_start_url", "sso_session"]) {
            if !names.contains(&p) {
                names.push(p);
            }
        }
    }
    match join(names) {
        Some(a) => (LoginState::LoggedIn, Some(a)),
        None => logged_out(),
    }
}

/// Stripe: `stripe/config.toml` `[default]` profile; account = `display_name`
/// (or `account_id`). API keys are never read (live keys may be in the Keychain).
fn stripe(env: &Env) -> Check {
    #[derive(Deserialize)]
    struct Cfg {
        default: Option<Profile>,
    }
    #[derive(Deserialize)]
    struct Profile {
        account_id: Option<String>,
        display_name: Option<String>,
        test_mode_api_key: Option<IgnoredAny>,
    }
    let path = env.xdg("XDG_CONFIG_HOME", ".config").join("stripe/config.toml");
    let cfg: Option<Cfg> = read(&path).and_then(|t| toml::from_str(&t).ok());
    match cfg.and_then(|c| c.default) {
        Some(p) if p.account_id.is_some() || p.test_mode_api_key.is_some() => {
            let account = p.display_name.filter(|s| !s.is_empty()).or(p.account_id);
            (LoginState::LoggedIn, account)
        }
        _ => logged_out(),
    }
}

/// Fly: a non-empty top-level `access_token:` in `~/.fly/config.yml` (presence only).
fn fly(env: &Env) -> Check {
    let Some(text) = read(&env.home().join(".fly/config.yml")) else { return logged_out() };
    let authed = lines(&text)
        .filter(|l| !l.starts_with([' ', '\t']))
        .any(|l| matches!(kv(l, ':'), Some(("access_token", v)) if !v.is_empty()));
    if authed { (LoginState::LoggedIn, None) } else { logged_out() }
}

/// Supabase: `~/.supabase/access-token` exists (file fallback; never opened).
/// The CLI prefers the system keyring, so no file means `Unknown`.
fn supabase(env: &Env) -> Check {
    if env.home().join(".supabase/access-token").is_file() {
        (LoginState::LoggedIn, None)
    } else {
        (LoginState::Unknown, None)
    }
}

/// Docker: registry names under `auths` in `config.json` (the `auth` values are
/// never read). With a `credsStore` and no entries the login may be in a keyring.
fn docker(env: &Env) -> Check {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Cfg {
        #[serde(default)]
        auths: HashMap<String, IgnoredAny>,
        creds_store: Option<String>,
    }
    let dir = env.path_var("DOCKER_CONFIG").unwrap_or_else(|| env.home().join(".docker"));
    let Some(cfg) = json::<Cfg>(&dir.join("config.json")) else { return logged_out() };
    let mut regs: Vec<String> = cfg
        .auths
        .into_keys()
        .map(|r| match r.as_str() {
            "https://index.docker.io/v1/" => "docker.io".into(),
            _ => r,
        })
        .collect();
    regs.sort();
    match join(regs) {
        Some(a) => (LoginState::LoggedIn, Some(a)),
        None if cfg.creds_store.is_some() => (LoginState::Unknown, None),
        None => logged_out(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Fresh fixture HOME under the system temp dir, removed on drop.
    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            static N: AtomicUsize = AtomicUsize::new(0);
            let dir = std::env::temp_dir().join(format!(
                "oui-accounts-test-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::SeqCst)
            ));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Fixture(dir)
        }

        fn write(&self, rel: &str, body: &str) -> &Self {
            let p = self.0.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, body).unwrap();
            self
        }

        fn env(&self, extra: &[(&str, &str)], macos: bool) -> Env {
            let mut vars = HashMap::from([
                ("HOME".to_string(), self.0.to_string_lossy().into_owned()),
                ("PATH".to_string(), self.0.join("bin").to_string_lossy().into_owned()),
            ]);
            for (k, v) in extra {
                vars.insert(k.to_string(), v.to_string());
            }
            Env { vars, macos }
        }

        fn status(&self, tool: &str, extra: &[(&str, &str)], macos: bool) -> ToolStatus {
            detect_env(&self.env(extra, macos)).into_iter().find(|s| s.tool == tool).unwrap()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    const SECRET: &str = "SECRET-DO-NOT-LEAK";

    fn assert_state(s: &ToolStatus, state: LoginState, account: Option<&str>) {
        assert_eq!((s.state, s.account.as_deref()), (state, account), "{}", s.tool);
        assert!(!format!("{s:?}").contains(SECRET), "{} leaked a secret", s.tool);
    }

    #[test]
    fn empty_home_is_logged_out_and_not_installed() {
        let f = Fixture::new();
        for s in detect_env(&f.env(&[], false)) {
            assert!(!s.installed, "{}", s.tool);
            let expected = if s.tool == "supabase" { LoginState::Unknown } else { LoginState::LoggedOut };
            assert_state(&s, expected, None);
        }
    }

    #[test]
    fn installed_means_executable_on_path() {
        let f = Fixture::new();
        f.write("bin/gh", "#!/bin/sh\n").write("bin/flyctl", "#!/bin/sh\n").write("bin/npm", "");
        fs::set_permissions(f.0.join("bin/gh"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(f.0.join("bin/flyctl"), fs::Permissions::from_mode(0o755)).unwrap();
        let env = f.env(&[], false);
        let all = detect_env(&env);
        let inst = |t: &str| all.iter().find(|s| s.tool == t).unwrap().installed;
        assert!(inst("gh") && inst("fly"));
        assert!(!inst("npm"), "non-executable file is not installed");
        assert!(!inst("railway"));
    }

    #[test]
    fn claude_oauth_account_email() {
        let f = Fixture::new();
        f.write(".claude.json", &format!(
            r#"{{"oauthAccount":{{"emailAddress":"a@example.com","accountUuid":"u"}},"projects":{{}},"x":"{SECRET}"}}"#
        ));
        assert_state(&f.status("claude", &[], true), LoginState::LoggedIn, Some("a@example.com"));
    }

    #[test]
    fn claude_config_dir_and_credentials_file() {
        let f = Fixture::new();
        f.write("p/.credentials.json", SECRET).write("p/.claude.json", "{}");
        let dir = f.0.join("p").to_string_lossy().into_owned();
        let env = [("CLAUDE_CONFIG_DIR", dir.as_str())];
        assert_state(&f.status("claude", &env, false), LoginState::LoggedIn, None);
        // ~/.claude.json is ignored when CLAUDE_CONFIG_DIR is set.
        f.write(".claude.json", r#"{"oauthAccount":{"emailAddress":"other@example.com"}}"#);
        assert_state(&f.status("claude", &env, false), LoginState::LoggedIn, None);
    }

    #[test]
    fn claude_without_marker_is_unknown_on_macos() {
        let f = Fixture::new();
        f.write(".claude.json", r#"{"numStartups":3}"#);
        assert_state(&f.status("claude", &[], true), LoginState::Unknown, None);
        assert_state(&f.status("claude", &[], false), LoginState::LoggedOut, None);
    }

    #[test]
    fn railway_token_presence() {
        let f = Fixture::new();
        f.write(".railway/config.json", &format!(r#"{{"projects":{{}},"user":{{"token":"{SECRET}"}}}}"#));
        assert_state(&f.status("railway", &[], false), LoginState::LoggedIn, None);
        f.write(".railway/config.json", r#"{"projects":{},"user":{}}"#);
        assert_state(&f.status("railway", &[], false), LoginState::LoggedOut, None);
    }

    #[test]
    fn gh_hosts_users_only() {
        let f = Fixture::new();
        f.write(".config/gh/hosts.yml", &format!(
            "github.com:\n    users:\n        octocat:\n            oauth_token: {SECRET}\n            user: nested\n    git_protocol: https\n    oauth_token: {SECRET}\n    user: octocat\nghe.corp.com:\n    user: \"jdoe\"\n"
        ));
        assert_state(&f.status("gh", &[], false), LoginState::LoggedIn, Some("octocat, jdoe@ghe.corp.com"));
        // GH_CONFIG_DIR wins over XDG.
        let other = f.0.join("ghdir").to_string_lossy().into_owned();
        assert_state(&f.status("gh", &[("GH_CONFIG_DIR", &other)], false), LoginState::LoggedOut, None);
        f.write(".config/gh/hosts.yml", "{}\n");
        assert_state(&f.status("gh", &[], false), LoginState::LoggedOut, None);
    }

    #[test]
    fn vercel_auth_token_in_data_dirs() {
        let f = Fixture::new();
        let auth = format!(r#"{{"// Note":"This is your Vercel credentials file.","token":"{SECRET}"}}"#);
        f.write("Library/Application Support/com.vercel.cli/auth.json", &auth);
        assert_state(&f.status("vercel", &[], true), LoginState::LoggedIn, None);
        assert_state(&f.status("vercel", &[], false), LoginState::LoggedOut, None);
        f.write("xdg/com.vercel.cli/auth.json", "{}");
        let xdg = f.0.join("xdg").to_string_lossy().into_owned();
        assert_state(&f.status("vercel", &[("XDG_DATA_HOME", &xdg)], true), LoginState::LoggedOut, None);
    }

    #[test]
    fn git_user_email() {
        let f = Fixture::new();
        f.write(".gitconfig", &format!(
            "[credential]\n\thelper = store\n[url \"https://x:{SECRET}@host\"]\n\tinsteadOf = y\n[User]\n\tname = Jo\n\tEmail = \"jo@example.com\" ; work\n"
        ));
        assert_state(&f.status("git", &[], false), LoginState::LoggedIn, Some("jo@example.com"));
        f.write("g/cfg", "[user]\nname = only\n");
        let g = f.0.join("g/cfg").to_string_lossy().into_owned();
        assert_state(&f.status("git", &[("GIT_CONFIG_GLOBAL", &g)], false), LoginState::LoggedOut, None);
    }

    #[test]
    fn npm_auth_token_key() {
        let f = Fixture::new();
        f.write(".npmrc", &format!("# c\nregistry=https://registry.npmjs.org/\n//registry.npmjs.org/:_authToken={SECRET}\n"));
        assert_state(&f.status("npm", &[], false), LoginState::LoggedIn, None);
        f.write(".npmrc", "save-exact=true\n");
        assert_state(&f.status("npm", &[], false), LoginState::LoggedOut, None);
    }

    #[test]
    fn aws_static_and_sso_profiles() {
        let f = Fixture::new();
        f.write(".aws/credentials", &format!("[default]\naws_access_key_id = AKIA\naws_secret_access_key = {SECRET}\n[empty]\n"));
        f.write(".aws/config", "[default]\nregion = eu-west-1\n[profile work]\nsso_session = corp\n");
        assert_state(&f.status("aws", &[], false), LoginState::LoggedIn, Some("default"));
        f.write(".aws/sso/cache/abc.json", &format!(r#"{{"accessToken":"{SECRET}"}}"#));
        assert_state(&f.status("aws", &[], false), LoginState::LoggedIn, Some("default, work"));
    }

    #[test]
    fn stripe_display_name() {
        let f = Fixture::new();
        f.write(".config/stripe/config.toml", &format!(
            "color = \"auto\"\n[default]\naccount_id = \"acct_1\"\ndisplay_name = \"Acme\"\ntest_mode_api_key = \"{SECRET}\"\n"
        ));
        assert_state(&f.status("stripe", &[], false), LoginState::LoggedIn, Some("Acme"));
        f.write(".config/stripe/config.toml", "[default]\ncolor = \"auto\"\n");
        assert_state(&f.status("stripe", &[], false), LoginState::LoggedOut, None);
    }

    #[test]
    fn fly_access_token_presence() {
        let f = Fixture::new();
        f.write(".fly/config.yml", &format!("access_token: {SECRET}\nsend_metrics: true\n"));
        assert_state(&f.status("fly", &[], false), LoginState::LoggedIn, None);
        f.write(".fly/config.yml", "access_token: \"\"\n");
        assert_state(&f.status("fly", &[], false), LoginState::LoggedOut, None);
    }

    #[test]
    fn supabase_token_file() {
        let f = Fixture::new();
        f.write(".supabase/access-token", SECRET);
        assert_state(&f.status("supabase", &[], false), LoginState::LoggedIn, None);
    }

    #[test]
    fn docker_registries() {
        let f = Fixture::new();
        f.write(".docker/config.json", &format!(
            r#"{{"auths":{{"https://index.docker.io/v1/":{{"auth":"{SECRET}"}},"ghcr.io":{{}}}},"credsStore":"desktop"}}"#
        ));
        assert_state(&f.status("docker", &[], false), LoginState::LoggedIn, Some("docker.io, ghcr.io"));
        f.write(".docker/config.json", r#"{"auths":{},"credsStore":"desktop"}"#);
        assert_state(&f.status("docker", &[], false), LoginState::Unknown, None);
        f.write(".docker/config.json", r#"{"auths":{}}"#);
        assert_state(&f.status("docker", &[], false), LoginState::LoggedOut, None);
    }

    #[test]
    fn env_token_presence_counts_without_value() {
        let f = Fixture::new();
        let s = f.status("railway", &[("RAILWAY_TOKEN", SECRET)], false);
        assert_state(&s, LoginState::LoggedIn, Some("$RAILWAY_TOKEN"));
        assert_state(&f.status("vercel", &[("VERCEL_TOKEN", "")], false), LoginState::LoggedOut, None);
    }

    #[test]
    fn malformed_files_do_not_panic() {
        let f = Fixture::new();
        for p in [".claude.json", ".railway/config.json", ".docker/config.json", ".config/stripe/config.toml",
                  ".config/gh/hosts.yml", ".gitconfig", ".npmrc", ".aws/credentials", ".fly/config.yml"] {
            f.write(p, "\u{0}{[ :=garbage");
        }
        assert_eq!(detect_env(&f.env(&[], true)).len(), TOOLS.len());
    }
}
