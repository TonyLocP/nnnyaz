//! Yazi-compatible "open with" menu.
//!
//! rnn has no configuration of its own, so this reuses the user's existing
//! Yazi config: the `[opener]` definitions and the `[open]` rules decide
//! which programs are offered for a given file. That keeps the menu in sync
//! with what the user already sees in Yazi.

use std::{
    collections::HashMap,
    env,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

/// One entry in the open-with menu.
#[derive(Clone)]
pub struct Opener {
    pub name: String,
    pub desc: Option<String>,
    pub command: String,
    pub block: bool,
}

impl Opener {
    /// Expand yazi's `%s` / `%s1` / `%d` / `%d1` placeholders for `path`.
    pub fn command_for(&self, path: &Path) -> String {
        let file = shell_quote(&path.to_string_lossy());
        let dir = shell_quote(&path.parent().unwrap_or(Path::new(".")).to_string_lossy());
        let mut cmd = self.command.clone();
        // Numbered forms first so `%s1` isn't consumed by `%s`.
        for (from, to) in [("%s1", &file), ("%s", &file), ("%d1", &dir), ("%d", &dir)] {
            cmd = cmd.replace(from, to);
        }
        cmd
    }
}

/// Build the open-with list for `path`.
///
/// `doc` is the `[opener]`/`[open]` part of rnn's `config.toml` (if any). It
/// replaces the matching section from the user's Yazi config; missing sections
/// still fall back to Yazi's config and then to built-in defaults.
pub fn options(path: &Path, doc: Option<&toml::Value>) -> Vec<Opener> {
    let mut merged = load_config().unwrap_or_else(|| toml::Value::Table(toml::value::Table::new()));
    if let Some(over) = doc.and_then(|d| d.as_table()) {
        if let Some(base) = merged.as_table_mut() {
            for key in ["opener", "open"] {
                if let Some(value) = over.get(key) {
                    base.insert(key.to_string(), value.clone());
                }
            }
        }
    }
    let doc = &merged;
    let mut openers = builtin_openers();
    if let Some(table) = doc.get("opener").and_then(|v| v.as_table()) {
        for (name, value) in table {
            if let Some(op) = parse_opener(name, value) {
                openers.insert(name.clone(), op);
            }
        }
    }

    let uses = matching_uses(doc, path);
    let mut out = Vec::new();
    for name in uses {
        if let Some(op) = openers.get(&name) {
            if !out.iter().any(|o: &Opener| o.name == op.name) {
                out.push(op.clone());
            }
        }
    }
    if out.is_empty() {
        fallback()
    } else {
        out
    }
}

fn fallback() -> Vec<Opener> {
    vec![
        Opener {
            name: "open".into(),
            desc: Some("system default".into()),
            command: "rnn:open".into(),
            block: false,
        },
        Opener {
            name: "edit".into(),
            desc: Some("$EDITOR".into()),
            command: "${EDITOR:-vi} %s".into(),
            block: true,
        },
    ]
}

fn default_opener() -> String {
    std::env::var("OPENER").unwrap_or_else(|_| {
        if cfg!(target_os = "macos") {
            "open".into()
        } else {
            "xdg-open".into()
        }
    })
}

fn builtin_openers() -> HashMap<String, Opener> {
    let mut m = HashMap::new();
    m.insert(
        "open".into(),
        Opener {
            name: "open".into(),
            desc: Some("system default".into()),
            command: "rnn:open".into(),
            block: false,
        },
    );
    m.insert(
        "play".into(),
        Opener {
            name: "play".into(),
            desc: Some("Play".into()),
            command: format!("{} %s1", default_opener()),
            block: false,
        },
    );
    m.insert(
        "reveal".into(),
        Opener {
            name: "reveal".into(),
            desc: Some("Reveal".into()),
            command: format!("{} %d1", default_opener()),
            block: false,
        },
    );
    m.insert(
        "edit".into(),
        Opener {
            name: "edit".into(),
            desc: Some("$EDITOR".into()),
            command: "${EDITOR:-vi} %s".into(),
            block: true,
        },
    );
    m
}

fn load_config() -> Option<toml::Value> {
    let text = std::fs::read_to_string(config_path()?).ok()?;
    toml::from_str(&text).ok()
}

fn config_path() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("YAZI_CONFIG_HOME") {
        let p = PathBuf::from(dir).join("yazi.toml");
        if p.is_file() {
            return Some(p);
        }
    }
    if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME") {
        let p = PathBuf::from(dir).join("yazi/yazi.toml");
        if p.is_file() {
            return Some(p);
        }
    }
    let home = std::env::var_os("HOME")?;
    let p = PathBuf::from(home).join(".config/yazi/yazi.toml");
    p.is_file().then_some(p)
}

fn parse_opener(name: &str, value: &toml::Value) -> Option<Opener> {
    let entries: Vec<&toml::Value> = match value {
        toml::Value::Array(a) => a.iter().collect(),
        toml::Value::Table(_) => vec![value],
        _ => return None,
    };
    for entry in entries {
        let table = entry.as_table()?;
        if !platform_ok(table.get("for").and_then(|v| v.as_str())) {
            continue;
        }
        let Some(run) = table.get("run").and_then(|v| v.as_str()) else {
            continue;
        };
        return Some(Opener {
            name: name.to_string(),
            desc: table
                .get("desc")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            command: run.to_string(),
            block: table
                .get("block")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        });
    }
    None
}

fn platform_ok(v: Option<&str>) -> bool {
    match v {
        None => true,
        Some("unix") => cfg!(unix),
        Some("linux") => cfg!(target_os = "linux"),
        Some("windows") => cfg!(windows),
        Some("macos") => cfg!(target_os = "macos"),
        Some("android") => cfg!(target_os = "android"),
        Some(_) => false,
    }
}

/// Yazi's preset `[open]` rules, used as the base when the user's config
/// doesn't replace them. Keep in sync with `yazi-config/preset/yazi-default.toml`.
const DEFAULT_RULES: &str = r#"
rules = [
	{ mime = "folder/*", use = [ "edit", "open", "reveal" ] },
	{ mime = "text/*", use = [ "edit", "reveal" ] },
	{ mime = "image/*", use = [ "open", "reveal" ] },
	{ mime = "{audio,video}/*", use = [ "play", "reveal" ] },
	{ mime = "application/{json,ndjson,javascript,wine-extension-ini}", use = [ "edit", "reveal" ] },
	{ mime = "application/{zip,rar,7z*,tar,gzip,xz,zstd,bzip*,lzma,compress,archive,cpio,arj,xar,ms-cab*}", use = [ "extract", "reveal" ] },
	{ mime = "inode/empty", use = [ "edit", "reveal" ] },
	{ mime = "vfs/{absent,stale}", use = "download" },
	{ mime = "trash/**", use = [ "open", "trash" ] },
	{ url = "*", use = [ "open", "reveal" ] },
]
"#;

/// A single `[open]` rule.
struct Rule<'a> {
    mime: Option<&'a str>,
    url: Option<&'a str>,
    uses: Vec<String>,
}

impl<'a> Rule<'a> {
    fn matches(&self, mime: Option<&str>, name: &str) -> bool {
        if let Some(pat) = self.mime {
            mime.is_some_and(|m| mime_matches(pat, m))
        } else if let Some(pat) = self.url {
            url_matches(pat, name)
        } else {
            false
        }
    }
}

fn rules_from(value: &toml::Value) -> Vec<Rule<'_>> {
    let Some(array) = value.as_array() else {
        return Vec::new();
    };
    array
        .iter()
        .filter_map(|rule| {
            let table = rule.as_table()?;
            Some(Rule {
                mime: table.get("mime").and_then(|v| v.as_str()),
                url: table.get("url").and_then(|v| v.as_str()),
                uses: use_list(table),
            })
        })
        .collect()
}

/// First `[open]` rule that matches `path`, returning its `use` list.
///
/// Order mirrors yazi: `prepend_rules`, then the base rules (the user's
/// `rules` when present, otherwise yazi's preset), then `append_rules`.
fn matching_uses(doc: &toml::Value, path: &Path) -> Vec<String> {
    let mime = if path.is_dir() {
        Some("folder/local".to_string())
    } else {
        file_mime(path)
    };
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();

    let defaults: toml::Value = toml::from_str(DEFAULT_RULES).expect("valid default rules");
    let mut rules: Vec<Rule> = Vec::new();
    if let Some(open) = doc.get("open").and_then(|v| v.as_table()) {
        if let Some(v) = open.get("prepend_rules") {
            rules.extend(rules_from(v));
        }
        if let Some(v) = open.get("rules") {
            rules.extend(rules_from(v));
        } else if let Some(v) = defaults.get("rules") {
            rules.extend(rules_from(v));
        }
        if let Some(v) = open.get("append_rules") {
            rules.extend(rules_from(v));
        }
    } else if let Some(v) = defaults.get("rules") {
        rules.extend(rules_from(v));
    }

    for rule in rules {
        if rule.matches(mime.as_deref(), &name) {
            return rule.uses;
        }
    }
    Vec::new()
}

fn use_list(table: &toml::value::Table) -> Vec<String> {
    match table.get("use") {
        Some(toml::Value::String(s)) => vec![s.clone()],
        Some(toml::Value::Array(a)) => a
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    }
}

/// Detect the file's MIME type with the same tools yazi relies on.
pub fn file_mime(path: &Path) -> Option<String> {
    let probes: [(&str, &[&str]); 2] = [
        ("xdg-mime", &["query", "filetype"]),
        ("file", &["-b", "--mime-type"]),
    ];
    for (prog, args) in probes {
        let Ok(out) = Command::new(prog).args(args).arg(path).output() else {
            continue;
        };
        if !out.status.success() {
            continue;
        }
        let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !text.is_empty() {
            return Some(text);
        }
    }
    None
}

/// The desktop file id of the current default web browser, e.g. `zen.desktop`.
///
/// `xdg-settings` is authoritative, but it can be unset; fall back to the
/// `http(s)` scheme handlers the way `xdg-settings` itself does.
pub fn default_browser() -> Option<String> {
    let probes: [(&str, &[&str]); 3] = [
        ("xdg-settings", &["get", "default-web-browser"]),
        ("xdg-mime", &["query", "default", "x-scheme-handler/https"]),
        ("xdg-mime", &["query", "default", "x-scheme-handler/http"]),
    ];
    for (prog, args) in probes {
        if let Some(id) = run_query(prog, args).and_then(|id| normalize_desktop_id(&id)) {
            // The shim below already opens the default browser; don't recurse.
            if id != BROWSER_SHIM {
                return Some(id);
            }
        }
    }
    None
}

/// Desktop file id of the registered handler for `mime`, e.g.
/// `brave-browser.desktop` for `application/pdf`.
pub fn default_handler(mime: &str) -> Option<String> {
    run_query("xdg-mime", &["query", "default", mime])
        .or_else(|| gio_default(mime))
        .and_then(|id| normalize_desktop_id(&id))
}

/// If `path` would open in a web browser that is not the current default
/// browser, return the default browser's desktop id and a `file://` URI.
///
/// This makes PDFs and other browser-associated formats follow the browser the
/// user picked with `xdg-settings`, instead of whatever browser the MIME
/// database happens to name. Explicit `OPENER` values bypass this entirely.
pub fn browser_redirect(path: &Path) -> Option<(String, String)> {
    let mime = file_mime(path)?;
    let handler = default_handler(&mime)?;
    if !desktop_is_browser(&handler) {
        return None;
    }
    let browser = default_browser()?;
    if browser == handler {
        return None;
    }
    Some((browser, file_uri(path)))
}

/// The `file://` URI for `path`, percent-encoding everything outside the
/// unreserved set so spaces and `#`/`?` survive the trip through `gtk-launch`.
pub fn file_uri(path: &Path) -> String {
    let absolute = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mut uri = String::from("file://");
    for &byte in absolute.as_os_str().as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                uri.push(byte as char);
            }
            _ => uri.push_str(&format!("%{byte:02X}")),
        }
    }
    uri
}

/// Launch `uri` with the desktop application `id` (e.g. `zen.desktop`).
///
/// Prefers `gtk-launch`, which takes desktop ids directly, and falls back to
/// `gio launch` with the resolved desktop file.
pub fn launch_desktop(id: &str, uri: &str) -> bool {
    if spawn_detached(Command::new("gtk-launch").arg(id).arg(uri)) {
        return true;
    }
    let Some(path) = desktop_file(id) else {
        return false;
    };
    spawn_detached(Command::new("gio").arg("launch").arg(path).arg(uri))
}

fn spawn_detached(cmd: &mut Command) -> bool {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .is_ok()
}

/// The wrapper desktop file that already routes to the default browser.
const BROWSER_SHIM: &str = "open-in-default-browser.desktop";

/// Run a query command and return its first non-empty output line.
fn run_query(prog: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(prog).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
    Some(line.to_string())
}

/// Parse `gio mime` output:
/// `Default application for “application/pdf”: brave-browser.desktop`.
fn gio_default(mime: &str) -> Option<String> {
    let out = Command::new("gio").args(["mime", mime]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().next()?.trim();
    let (_, id) = line.rsplit_once(':')?;
    let id = id.trim();
    id.ends_with(".desktop").then(|| id.to_string())
}

/// Accept `zen.desktop` as well as a bare `zen` when that desktop file exists.
fn normalize_desktop_id(id: &str) -> Option<String> {
    let id = id.trim();
    if id.is_empty() {
        return None;
    }
    if id.ends_with(".desktop") || id.contains('/') {
        return Some(id.to_string());
    }
    let with_ext = format!("{id}.desktop");
    desktop_file(&with_ext).is_some().then_some(with_ext)
}

/// Locate a desktop file by id in the XDG application directories.
fn desktop_file(id: &str) -> Option<PathBuf> {
    let path = Path::new(id);
    if path.is_absolute() {
        return path.is_file().then(|| path.to_path_buf());
    }

    let mut dirs = Vec::new();
    match env::var_os("XDG_DATA_HOME") {
        Some(dir) => dirs.push(PathBuf::from(dir).join("applications")),
        None => {
            if let Some(home) = env::var_os("HOME") {
                dirs.push(PathBuf::from(home).join(".local/share/applications"));
            }
        }
    }
    let data_dirs = env::var_os("XDG_DATA_DIRS")
        .map(|v| v.to_string_lossy().into_owned())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    for dir in data_dirs.split(':').filter(|d| !d.is_empty()) {
        dirs.push(PathBuf::from(dir).join("applications"));
    }
    // Flatpak exports are not always listed in XDG_DATA_DIRS.
    dirs.push(PathBuf::from("/var/lib/flatpak/exports/share/applications"));
    if let Some(home) = env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join(".local/share/flatpak/exports/share/applications"));
    }

    dirs.into_iter()
        .map(|dir| dir.join(id))
        .find(|candidate| candidate.is_file())
}

/// Whether the desktop file declares itself a web browser.
fn desktop_is_browser(id: &str) -> bool {
    let Some(path) = desktop_file(id) else {
        return false;
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    let (categories, mime_types) = desktop_entry_keys(&text);
    categories.iter().any(|c| c == "WebBrowser")
        || mime_types
            .iter()
            .any(|m| m == "x-scheme-handler/http" || m == "x-scheme-handler/https")
}

/// Read the `Categories=` and `MimeType=` values from the `[Desktop Entry]`
/// group. Keys in action groups are ignored.
fn desktop_entry_keys(text: &str) -> (Vec<String>, Vec<String>) {
    let mut categories = Vec::new();
    let mut mime_types = Vec::new();
    let mut in_main = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_main = line == "[Desktop Entry]";
            continue;
        }
        if !in_main {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let values = || {
            value
                .split(';')
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        };
        match key.trim() {
            "Categories" => categories = values(),
            "MimeType" => mime_types = values(),
            _ => {}
        }
    }
    (categories, mime_types)
}

fn mime_matches(pattern: &str, mime: &str) -> bool {
    expand_braces(pattern).iter().any(|p| glob_match(p, mime))
}

fn url_matches(pattern: &str, name: &str) -> bool {
    expand_braces(pattern).iter().any(|p| glob_match(p, name))
}

/// Expand the `{a,b}` alternation yazi uses in MIME/URL patterns.
fn expand_braces(pattern: &str) -> Vec<String> {
    let Some(open) = pattern.find('{') else {
        return vec![pattern.to_string()];
    };
    let Some(close_rel) = pattern[open..].find('}') else {
        return vec![pattern.to_string()];
    };
    let close = open + close_rel;
    let prefix = &pattern[..open];
    let suffix = &pattern[close + 1..];
    let mut out = Vec::new();
    for alt in pattern[open + 1..close].split(',') {
        let combined = format!("{prefix}{alt}{suffix}");
        out.extend(expand_braces(&combined));
    }
    out
}

/// Small glob matcher supporting `*` and `?`.
fn glob_match(pattern: &str, text: &str) -> bool {
    fn rec(p: &[char], t: &[char]) -> bool {
        match p.first() {
            None => t.is_empty(),
            Some('*') => (0..=t.len()).any(|i| rec(&p[1..], &t[i..])),
            Some('?') => !t.is_empty() && rec(&p[1..], &t[1..]),
            Some(c) => t.first() == Some(c) && rec(&p[1..], &t[1..]),
        }
    }
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    rec(&p, &t)
}

fn shell_quote(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:@,+".contains(c))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn braces_and_globs() {
        assert!(mime_matches("text/*", "text/plain"));
        assert!(mime_matches("image/{avif,hei?,jxl}", "image/heic"));
        assert!(mime_matches("{audio,video}/*", "video/mp4"));
        assert!(!mime_matches("text/*", "image/png"));
    }

    #[test]
    fn url_patterns() {
        assert!(url_matches("*.osz", "song.osz"));
        assert!(url_matches("*.{doc,docx}", "notes.docx"));
        assert!(!url_matches("*.osz", "song.osz2"));
    }

    #[test]
    fn expands_placeholders() {
        let op = Opener {
            name: "x".into(),
            desc: None,
            command: "prog %s1 %s %d1".into(),
            block: false,
        };
        let cmd = op.command_for(Path::new("/tmp/a b.txt"));
        assert_eq!(cmd, "prog '/tmp/a b.txt' '/tmp/a b.txt' /tmp");
    }

    #[test]
    fn folders_use_yazi_default_rule() {
        let doc: toml::Value = toml::from_str("").unwrap();
        let uses = matching_uses(&doc, Path::new("/tmp"));
        assert_eq!(uses, vec!["edit", "open", "reveal"]);
    }

    #[test]
    fn user_rules_replace_defaults_but_prepend_still_wins() {
        let doc: toml::Value = toml::from_str(
            r#"
            [open]
            prepend_rules = [{ url = "*", use = "first" }]
            rules = [{ mime = "folder/*", use = "custom" }]
            "#,
        )
        .unwrap();
        assert_eq!(matching_uses(&doc, Path::new("/tmp")), vec!["first"]);

        let doc: toml::Value = toml::from_str(
            r#"
            [open]
            rules = [{ mime = "folder/*", use = "custom" }]
            "#,
        )
        .unwrap();
        assert_eq!(matching_uses(&doc, Path::new("/tmp")), vec!["custom"]);
    }

    #[test]
    fn quotes_shell_specials() {
        assert_eq!(shell_quote("plain-name_1.txt"), "plain-name_1.txt");
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn parses_desktop_entry_categories() {
        let text = "\
[Desktop Entry]
Name=Zen
Categories=Network;WebBrowser;
MimeType=text/html;x-scheme-handler/https;

[Desktop Action new]
Categories=Other;
MimeType=application/pdf;
";
        let (categories, mime_types) = desktop_entry_keys(text);
        assert_eq!(categories, vec!["Network", "WebBrowser"]);
        assert_eq!(mime_types, vec!["text/html", "x-scheme-handler/https"]);
    }

    #[test]
    fn file_uris_encode_special_characters() {
        assert_eq!(
            file_uri(Path::new("/tmp/a b#c?.pdf")),
            "file:///tmp/a%20b%23c%3F.pdf"
        );
    }

    #[test]
    fn builtin_open_uses_the_internal_opener() {
        let openers = builtin_openers();
        assert_eq!(openers["open"].command, "rnn:open");
        assert_eq!(fallback()[0].command, "rnn:open");
    }
}
