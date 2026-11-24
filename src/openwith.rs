//! Yazi-compatible "open with" menu.
//!
//! nnnyaz has no configuration of its own, so this reuses the user's existing
//! Yazi config: the `[opener]` definitions and the `[open]` rules decide
//! which programs are offered for a given file. That keeps the menu in sync
//! with what the user already sees in Yazi.

use std::{
    collections::{HashMap, HashSet},
    env,
    iter::Peekable,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    str::Chars,
};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

/// One entry in the open-with menu.
#[derive(Clone)]
pub struct Opener {
    pub name: String,
    pub desc: Option<String>,
    pub command: String,
    pub block: bool,
}

impl Opener {
    /// Whether the command wants every target at once (an unnumbered `%s` or
    /// `%S`), mirroring yazi's `Splatter::spread`. Otherwise yazi runs the
    /// opener once per file.
    pub fn spread(&self) -> bool {
        command_spreads(&self.command)
    }

    /// Expand yazi's opener placeholders for `files`.
    ///
    /// Yazi splats an opener over just its target files: `%s`/`%S` are the
    /// targets and `%d`/`%D` their parent directories. Hovered, yanked, and
    /// tab placeholders expand to empty because an opener has no such context.
    pub fn command_for(&self, files: &[PathBuf]) -> String {
        let mut out = String::new();
        let mut it = self.command.chars().peekable();
        expand_all(&mut it, files, &mut out);
        out
    }

    /// Label for the interactive menu. Falls back to the first word of the
    /// command, like yazi's `OpenerRule::desc`.
    pub fn label(&self) -> String {
        match self.desc.as_deref() {
            Some(d) if !d.is_empty() => d.to_string(),
            _ => self
                .command
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .to_string(),
        }
    }
}

/// Walk the command and expand every `%` placeholder.
fn expand_all(it: &mut Peekable<Chars<'_>>, files: &[PathBuf], out: &mut String) {
    while let Some(c) = it.next() {
        if c == '%' {
            if it.peek().is_some() {
                expand_one(it, files, out);
            } else {
                out.push('%');
            }
        } else {
            out.push(c);
        }
    }
}

/// Expand the placeholder immediately after a `%`.
fn expand_one(it: &mut Peekable<Chars<'_>>, files: &[PathBuf], out: &mut String) {
    let Some(c) = it.next() else {
        out.push('%');
        return;
    };
    match c {
        's' | 'S' => expand_selected(it, files, out, false),
        'd' | 'D' => expand_selected(it, files, out, true),
        // An opener has no hovered file, so yazi emits an empty argument.
        'h' | 'H' => out.push_str("''"),
        // Likewise there is no yank register in an opener's context. A
        // numbered form still becomes an empty argument.
        'y' | 'Y' => {
            if consume_digits(it).is_some() {
                out.push_str("''");
            }
        }
        // `%t`/`%T` look the next placeholder up in a neighbouring tab. An
        // opener has no tab context, so that tab is empty.
        't' | 'T' => {
            if it.peek().is_some() {
                expand_one(it, &[], out);
            } else {
                out.push('%');
            }
        }
        '%' => out.push('%'),
        other => {
            out.push('%');
            out.push(other);
        }
    }
}

/// Expand `%s`/`%S` (files) or `%d`/`%D` (their directories). A trailing
/// number is 1-based; an out-of-range index becomes an empty argument, while
/// `0` behaves like the unnumbered form.
fn expand_selected(
    it: &mut Peekable<Chars<'_>>,
    files: &[PathBuf],
    out: &mut String,
    dirname: bool,
) {
    let idx = consume_digits(it);
    let push = |out: &mut String, p: &Path| out.push_str(&shell_quote(&p.to_string_lossy()));
    match idx {
        Some(0) if files.is_empty() => out.push_str("''"),
        Some(0) => {
            for (i, f) in files.iter().enumerate() {
                if i > 0 {
                    out.push(' ');
                }
                push(out, path_for(f, dirname));
            }
        }
        Some(n) => match files.get(n - 1) {
            Some(f) => push(out, path_for(f, dirname)),
            None => out.push_str("''"),
        },
        None => {
            for (i, f) in files.iter().enumerate() {
                if i > 0 {
                    out.push(' ');
                }
                push(out, path_for(f, dirname));
            }
        }
    }
}

/// The file itself, or its parent directory for `%d`/`%D`.
fn path_for(f: &Path, dirname: bool) -> &Path {
    if dirname {
        f.parent().unwrap_or(Path::new(""))
    } else {
        f
    }
}

/// Consume a run of ASCII digits after a placeholder letter. Returns `None`
/// when there are no digits (or the number overflows), matching yazi.
fn consume_digits(it: &mut Peekable<Chars<'_>>) -> Option<usize> {
    let mut value: Option<usize> = None;
    while let Some(c) = it.peek().copied() {
        let Some(d) = c.to_digit(10) else {
            break;
        };
        it.next();
        value = Some(
            value
                .unwrap_or(0)
                .checked_mul(10)?
                .checked_add(d as usize)?,
        );
    }
    value
}

/// True when the command contains an unnumbered `%s`/`%S` (including after a
/// `%t`/`%T` tab prefix), i.e. yazi would run it once with all targets.
fn command_spreads(command: &str) -> bool {
    let mut it = command.chars().peekable();
    while let Some(c) = it.next() {
        if c != '%' {
            continue;
        }
        let Some(p) = it.next() else {
            break;
        };
        if scan_placeholder(&mut it, p) {
            return true;
        }
    }
    false
}

/// Inspect one placeholder letter (already consumed). Returns true when it is
/// an unnumbered `%s`/`%S`, possibly reached through a tab prefix.
fn scan_placeholder(it: &mut Peekable<Chars<'_>>, p: char) -> bool {
    match p {
        's' | 'S' => {
            if !it.peek().is_some_and(|c| c.is_ascii_digit()) {
                return true;
            }
            let _ = consume_digits(it);
        }
        'd' | 'D' | 'y' | 'Y' => {
            let _ = consume_digits(it);
        }
        't' | 'T' => {
            if let Some(next) = it.next() {
                return scan_placeholder(it, next);
            }
        }
        _ => {}
    }
    false
}

/// Build the open-with list for a group of targets.
///
/// `doc` is the `[opener]`/`[open]` part of nnnyaz's `config.toml` (if any). It
/// replaces the matching section from the user's Yazi config; missing sections
/// still fall back to Yazi's config and then to built-in defaults. Like yazi's
/// `Open::match_common`, only openers offered by every target's `[open]` rule
/// are listed, in the first rule's order.
pub fn options_for(paths: &[PathBuf], doc: Option<&toml::Value>) -> Vec<Opener> {
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

    let rules = build_rules(doc);
    let mut uses: Option<Vec<String>> = None;
    for path in paths {
        let path_uses = matching_uses(&rules, path);
        if path_uses.is_empty() {
            uses = Some(Vec::new());
            break;
        }
        uses = Some(match uses {
            None => path_uses,
            Some(common) => common
                .into_iter()
                .filter(|name| path_uses.contains(name))
                .collect(),
        });
        if uses.as_ref().is_some_and(Vec::is_empty) {
            break;
        }
    }

    let mut out = Vec::new();
    for name in uses.unwrap_or_default() {
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

/// Append installed desktop applications that can open every target.
///
/// Configured openers are kept first and are not duplicated. A desktop entry
/// counts when its `MimeType=` list matches the file's MIME type and its
/// `TryExec`/`Exec` program is available. Entries without `MimeType=` use the
/// desktop-spec default of `text/plain`, broadened to `text/*` for text
/// editors. Hidden and `NoDisplay` entries are skipped, and the result is
/// sorted by display name.
pub fn available_apps_for(paths: &[PathBuf], configured: &[Opener]) -> Vec<Opener> {
    let mut mimes = Vec::new();
    for path in paths {
        let Some(mime) = open_mime(path) else {
            return Vec::new();
        };
        mimes.push(mime);
    }
    if mimes.is_empty() {
        return Vec::new();
    }
    desktop_openers_in(&desktop_dirs(), &mimes, configured)
}

fn desktop_openers_in(dirs: &[PathBuf], mimes: &[String], configured: &[Opener]) -> Vec<Opener> {
    let configured_names: HashSet<String> =
        configured.iter().map(|op| match_key(&op.name)).collect();
    let mut used_binaries: HashSet<String> = configured
        .iter()
        .filter_map(|op| command_binary(&op.command))
        .map(|binary| match_key(&binary))
        .collect();
    let mut configured_ids = HashSet::new();
    for op in configured {
        for word in shell_words(&op.command) {
            if word.ends_with(".desktop") {
                if let Some(id) = Path::new(&word).file_name() {
                    configured_ids.insert(id.to_string_lossy().into_owned());
                }
            }
        }
    }
    let mut configured_keys = configured_names;
    configured_keys.extend(used_binaries.iter().cloned());
    let mut seen_ids = HashSet::new();
    let mut out = Vec::new();

    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("desktop") {
                continue;
            }
            let Some(app) = parse_desktop_app(&path) else {
                continue;
            };
            if app.hidden || app.no_display {
                continue;
            }
            if !app.declares_mime && !accepts_files(&app.exec) {
                continue;
            }
            let matches = app
                .mime_types
                .iter()
                .any(|pattern| mimes.iter().all(|mime| mime_matches(pattern, mime)));
            if !matches {
                continue;
            }
            let Some(program) = command_program(&app.exec) else {
                continue;
            };
            let Some(binary) = command_binary(&app.exec) else {
                continue;
            };
            let check = app.try_exec.as_deref().unwrap_or(&program);
            if !executable_exists(check) {
                continue;
            }
            let id_key = match_key(app.id.trim_end_matches(".desktop"));
            if configured_keys.contains(&match_key(&app.name))
                || configured_keys.contains(&id_key)
                || configured_ids.contains(&app.id)
                || !used_binaries.insert(match_key(&binary))
                || !seen_ids.insert(app.id.clone())
            {
                continue;
            }
            let command = desktop_command(&app.exec, &app.name, &app.id);
            out.push(Opener {
                name: app.name,
                desc: Some(app.id),
                command,
                block: false,
            });
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.desc.cmp(&b.desc)));
    out
}

struct DesktopApp {
    id: String,
    name: String,
    exec: String,
    try_exec: Option<String>,
    mime_types: Vec<String>,
    categories: Vec<String>,
    declares_mime: bool,
    hidden: bool,
    no_display: bool,
}

fn parse_desktop_app(path: &Path) -> Option<DesktopApp> {
    let text = std::fs::read_to_string(path).ok()?;
    let id = path.file_name()?.to_string_lossy().into_owned();
    let mut app = DesktopApp {
        id,
        name: String::new(),
        exec: String::new(),
        try_exec: None,
        mime_types: Vec::new(),
        categories: Vec::new(),
        declares_mime: false,
        hidden: false,
        no_display: false,
    };
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
        let value = value.trim();
        match key.trim() {
            "Name" => app.name = value.to_string(),
            "Exec" => app.exec = value.to_string(),
            "TryExec" if !value.is_empty() => app.try_exec = Some(value.to_string()),
            "MimeType" => {
                app.declares_mime = true;
                app.mime_types = value
                    .split(';')
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .map(str::to_string)
                    .collect();
            }
            "Categories" => {
                app.categories = value
                    .split(';')
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .map(str::to_string)
                    .collect();
            }
            "Hidden" => app.hidden = desktop_bool(value),
            "NoDisplay" => app.no_display = desktop_bool(value),
            _ => {}
        }
    }
    if app.name.is_empty() {
        app.name = app.id.trim_end_matches(".desktop").to_string();
    }
    if app.exec.is_empty() {
        return None;
    }
    if app.mime_types.is_empty() {
        if app.declares_mime {
            return None;
        }
        // The desktop-entry spec assumes `text/plain` when `MimeType=` is
        // absent. Treat explicit text editors as supporting other text types
        // too, which is what users expect from editors such as Doom Emacs.
        app.mime_types.push(if is_text_editor(&app) {
            "text/*".to_string()
        } else {
            "text/plain".to_string()
        });
    }
    Some(app)
}

fn is_text_editor(app: &DesktopApp) -> bool {
    app.categories
        .iter()
        .any(|category| category.eq_ignore_ascii_case("TextEditor"))
}

/// Whether an `Exec=` line has a field code that passes a file to the app.
/// Required before applying the implicit `text/plain` default.
fn accepts_files(exec: &str) -> bool {
    let mut chars = exec.chars();
    while let Some(c) = chars.next() {
        if c == '%'
            && matches!(
                chars.next(),
                Some('f' | 'F' | 'u' | 'U' | 'd' | 'D' | 'n' | 'N')
            )
        {
            return true;
        }
    }
    false
}

fn desktop_bool(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes"
    )
}

/// Convert a desktop `Exec=` line to a Yazi-style opener command.
///
/// `%f`/`%u` become a numbered file placeholder, `%F`/`%U` become the
/// all-targets placeholder, and `%c`/`%k` are filled from the entry itself.
/// Unknown field codes are dropped, as required by the desktop spec.
fn desktop_command(exec: &str, name: &str, id: &str) -> String {
    let mut out = String::new();
    let mut chars = exec.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        let Some(code) = chars.next() else {
            out.push('%');
            break;
        };
        match code {
            'f' | 'u' => out.push_str("%s1"),
            'F' | 'U' => out.push_str("%s"),
            'c' => out.push_str(&shell_quote(name)),
            'k' => out.push_str(&shell_quote(id)),
            '%' => out.push('%'),
            _ => {}
        }
    }
    out
}

/// Comparison key for names, binaries, and desktop ids, so `brave_browser` and
/// `brave-browser.desktop` match.
fn match_key(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// First executable program in a shell command, ignoring `env` assignments.
/// Keeps any path component so absolute `Exec=` programs can be verified.
fn command_program(command: &str) -> Option<String> {
    let mut words = shell_words(command).into_iter();
    let mut first = words.next()?;
    if first == "env" {
        for word in words.by_ref() {
            if word == "--" {
                break;
            }
            if word.starts_with('-') || word.contains('=') {
                continue;
            }
            first = word;
            break;
        }
    }
    Some(first)
}

/// Lowercase basename used to dedupe configured and detected openers.
fn command_binary(command: &str) -> Option<String> {
    let program = command_program(command)?;
    Path::new(&program)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned().to_ascii_lowercase())
}

/// Minimal shell word splitter for the first token of `Exec=`/`run=` values.
fn shell_words(command: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut escaped = false;
    let mut started = false;
    for c in command.chars() {
        if escaped {
            current.push(c);
            escaped = false;
            started = true;
            continue;
        }
        match quote {
            Some('\'') => {
                if c == '\'' {
                    quote = None;
                } else {
                    current.push(c);
                }
            }
            Some('"') => match c {
                '"' => quote = None,
                '\\' => escaped = true,
                _ => current.push(c),
            },
            _ => match c {
                '\'' | '"' => {
                    quote = Some(c);
                    started = true;
                }
                '\\' => {
                    escaped = true;
                    started = true;
                }
                c if c.is_whitespace() => {
                    if started {
                        words.push(std::mem::take(&mut current));
                        started = false;
                    }
                }
                _ => {
                    current.push(c);
                    started = true;
                }
            },
        }
    }
    if started {
        words.push(current);
    }
    words
}

/// Whether `spec` names an executable file, either directly or through `PATH`.
fn executable_exists(spec: &str) -> bool {
    let path = Path::new(spec);
    if path.components().count() > 1 {
        return is_executable_file(path);
    }
    let Some(paths) = env::var_os("PATH") else {
        return false;
    };
    env::split_paths(&paths).any(|dir| is_executable_file(&dir.join(spec)))
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
        && std::fs::metadata(path).is_ok_and(|meta| meta.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

fn fallback() -> Vec<Opener> {
    vec![
        Opener {
            name: "open".into(),
            desc: Some("system default".into()),
            command: "nnnyaz:open".into(),
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
            command: "nnnyaz:open".into(),
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
struct Rule {
    mime: Option<String>,
    url: Option<String>,
    uses: Vec<String>,
}

impl Rule {
    fn matches(&self, mime: Option<&str>, name: &str) -> bool {
        if let Some(pat) = &self.mime {
            mime.is_some_and(|m| mime_matches(pat, m))
        } else if let Some(pat) = &self.url {
            url_matches(pat, name)
        } else {
            false
        }
    }
}

fn rules_from(value: &toml::Value) -> Vec<Rule> {
    let Some(array) = value.as_array() else {
        return Vec::new();
    };
    array
        .iter()
        .filter_map(|rule| {
            let table = rule.as_table()?;
            Some(Rule {
                mime: table
                    .get("mime")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                url: table
                    .get("url")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                uses: use_list(table),
            })
        })
        .collect()
}

/// Assemble the `[open]` rule list once for a group of targets.
///
/// Order mirrors yazi: `prepend_rules`, then the base rules (the user's
/// `rules` when present, otherwise yazi's preset), then `append_rules`.
fn build_rules(doc: &toml::Value) -> Vec<Rule> {
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
    rules
}

/// MIME type used for open-with rules and desktop scan matching.
fn open_mime(path: &Path) -> Option<String> {
    if path.is_dir() {
        Some("folder/local".to_string())
    } else {
        file_mime(path)
    }
}

/// First `[open]` rule that matches `path`, returning its `use` list.
fn matching_uses(rules: &[Rule], path: &Path) -> Vec<String> {
    let mime = open_mime(path);
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();

    for rule in rules {
        if rule.matches(mime.as_deref(), &name) {
            return rule.uses.clone();
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

/// XDG application directories, user entries first and without duplicates.
fn desktop_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    match env::var_os("XDG_DATA_HOME") {
        Some(dir) => push_dir(&mut dirs, PathBuf::from(dir).join("applications")),
        None => {
            if let Some(home) = env::var_os("HOME") {
                push_dir(
                    &mut dirs,
                    PathBuf::from(home).join(".local/share/applications"),
                );
            }
        }
    }
    let data_dirs = env::var_os("XDG_DATA_DIRS")
        .map(|v| v.to_string_lossy().into_owned())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    for dir in data_dirs.split(':').filter(|d| !d.is_empty()) {
        push_dir(&mut dirs, PathBuf::from(dir).join("applications"));
    }
    // Flatpak and snap exports are not always listed in XDG_DATA_DIRS.
    push_dir(
        &mut dirs,
        PathBuf::from("/var/lib/flatpak/exports/share/applications"),
    );
    if let Some(home) = env::var_os("HOME") {
        push_dir(
            &mut dirs,
            PathBuf::from(home).join(".local/share/flatpak/exports/share/applications"),
        );
    }
    push_dir(
        &mut dirs,
        PathBuf::from("/var/lib/snapd/desktop/applications"),
    );
    dirs
}

fn push_dir(dirs: &mut Vec<PathBuf>, dir: PathBuf) {
    if !dirs.contains(&dir) {
        dirs.push(dir);
    }
}

/// Locate a desktop file by id in the XDG application directories.
fn desktop_file(id: &str) -> Option<PathBuf> {
    let path = Path::new(id);
    if path.is_absolute() {
        return path.is_file().then(|| path.to_path_buf());
    }
    desktop_dirs()
        .into_iter()
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
    fn expands_placeholders_like_yazi() {
        let files = vec![PathBuf::from("/tmp/a"), PathBuf::from("/tmp/b c")];
        let cases = [
            ("prog %s", "prog /tmp/a '/tmp/b c'"),
            ("prog %s1 %s2 %s3", "prog /tmp/a '/tmp/b c' ''"),
            (
                "prog %s %s2 %s",
                "prog /tmp/a '/tmp/b c' '/tmp/b c' /tmp/a '/tmp/b c'",
            ),
            ("prog %d", "prog /tmp /tmp"),
            ("prog %d1 %d3 %d2", "prog /tmp '' /tmp"),
            ("prog %S", "prog /tmp/a '/tmp/b c'"),
            ("prog %s0", "prog /tmp/a '/tmp/b c'"),
            ("prog %% %s2 %x", "prog % '/tmp/b c' %x"),
            ("prog %", "prog %"),
        ];
        for (command, expected) in cases {
            assert_eq!(op(command).command_for(&files), expected, "{command}");
        }
        assert_eq!(op("prog %s").command_for(&[]), "prog ");
        assert_eq!(op("prog %s1").command_for(&[]), "prog ''");
    }

    #[test]
    fn opener_context_has_no_hovered_or_yanked_files() {
        // Yazi splats an opener over just its targets, so these placeholders
        // expand to empty (or an empty argument) rather than the manager state.
        assert_eq!(op("prog %h %H").command_for(&[]), "prog '' ''");
        assert_eq!(op("prog %y %Y").command_for(&[]), "prog  ");
        assert_eq!(op("prog %y1").command_for(&[]), "prog ''");
        assert_eq!(op("prog %ts").command_for(&[]), "prog ");
        assert_eq!(op("prog %ts1").command_for(&[]), "prog ''");
    }

    #[test]
    fn spread_matches_yazi() {
        assert!(op("%s").spread());
        assert!(op("%S").spread());
        assert!(!op("%s1").spread());
        assert!(!op("%%s").spread());
        assert!(op("%ts").spread());
        assert!(!op("%Ts1").spread());
        assert!(!op("prog").spread());
        assert!(op("%d %s").spread());
    }

    #[test]
    fn opener_label_falls_back_to_the_command() {
        assert_eq!(op("mpv %s").label(), "mpv");
        let mut with_desc = op("mpv %s");
        with_desc.desc = Some("Play".into());
        assert_eq!(with_desc.label(), "Play");
    }

    fn op(command: &str) -> Opener {
        Opener {
            name: "x".into(),
            desc: None,
            command: command.into(),
            block: false,
        }
    }

    #[test]
    fn desktop_exec_codes_become_opener_placeholders() {
        assert_eq!(
            desktop_command("viewer -n %f", "Viewer", "viewer.desktop"),
            "viewer -n %s1"
        );
        assert_eq!(
            desktop_command("viewer %F", "Viewer", "v.desktop"),
            "viewer %s"
        );
        assert_eq!(
            desktop_command("viewer %c %k %%", "A B", "v.desktop"),
            "viewer 'A B' v.desktop %"
        );
    }

    #[test]
    fn command_binary_skips_env_assignments() {
        assert_eq!(command_binary("viewer %f").as_deref(), Some("viewer"));
        assert_eq!(
            command_binary("env FOO=1 /usr/bin/feh %f").as_deref(),
            Some("feh")
        );
        assert_eq!(
            command_program("env FOO=1 /usr/bin/feh %f").as_deref(),
            Some("/usr/bin/feh")
        );
        assert_eq!(match_key("brave_browser"), match_key("brave-browser"));
    }

    #[test]
    fn desktop_scan_filters_by_mime_and_deduplicates() {
        let dir = std::env::temp_dir().join(format!(
            "nnnyaz-desktop-scan-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("viewer.desktop"),
            "[Desktop Entry]\nName=Viewer\nExec=viewer %f\nTryExec=true\nMimeType=image/png;\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("hidden.desktop"),
            "[Desktop Entry]\nName=Hidden\nExec=hidden %f\nTryExec=true\nMimeType=image/png;\nHidden=true\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("text.desktop"),
            "[Desktop Entry]\nName=Text\nExec=text %f\nTryExec=true\nMimeType=text/plain;\n",
        )
        .unwrap();

        let found = desktop_openers_in(std::slice::from_ref(&dir), &["image/png".into()], &[]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "Viewer");
        assert_eq!(found[0].desc.as_deref(), Some("viewer.desktop"));
        assert_eq!(found[0].command, "viewer %s1");

        let configured = vec![op("viewer %s")];
        assert!(desktop_openers_in(
            std::slice::from_ref(&dir),
            &["image/png".into()],
            &configured
        )
        .is_empty());
        let configured_desktop = vec![op("gtk-launch viewer.desktop %s1")];
        assert!(desktop_openers_in(
            std::slice::from_ref(&dir),
            &["image/png".into()],
            &configured_desktop
        )
        .is_empty());
        assert!(desktop_openers_in(
            std::slice::from_ref(&dir),
            &["image/png".into(), "text/plain".into()],
            &[]
        )
        .is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn desktop_entries_without_mime_type_use_text_defaults() {
        let dir = std::env::temp_dir().join(format!(
            "nnnyaz-desktop-implicit-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let bin = |name: &str| {
            let path = dir.join(name);
            std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
            #[cfg(unix)]
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path.to_string_lossy().into_owned()
        };
        let editor = bin("editor-bin");
        let plain = bin("plain-bin");
        let nofiles = bin("nofiles-bin");
        let empty = bin("empty-bin");
        std::fs::write(
            dir.join("editor.desktop"),
            format!(
                "[Desktop Entry]\nName=Editor\nExec={editor} %F\nCategories=Development;TextEditor;\n"
            ),
        )
        .unwrap();
        std::fs::write(
            dir.join("plain.desktop"),
            format!("[Desktop Entry]\nName=Plain\nExec={plain} %F\n"),
        )
        .unwrap();
        std::fs::write(
            dir.join("nofiles.desktop"),
            format!("[Desktop Entry]\nName=NoFiles\nExec={nofiles}\n"),
        )
        .unwrap();
        std::fs::write(
            dir.join("empty.desktop"),
            format!("[Desktop Entry]\nName=Empty\nExec={empty} %F\nMimeType=\n"),
        )
        .unwrap();

        let source = desktop_openers_in(std::slice::from_ref(&dir), &["text/x-c".into()], &[]);
        assert_eq!(source.len(), 1);
        assert_eq!(source[0].name, "Editor");
        assert_eq!(source[0].command, format!("{editor} %s"));

        let text = desktop_openers_in(std::slice::from_ref(&dir), &["text/plain".into()], &[]);
        assert_eq!(text.len(), 2);
        assert_eq!(text[0].name, "Editor");
        assert_eq!(text[1].name, "Plain");

        assert!(
            desktop_openers_in(std::slice::from_ref(&dir), &["image/png".into()], &[]).is_empty()
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn folders_use_yazi_default_rule() {
        let doc: toml::Value = toml::from_str("").unwrap();
        let uses = matching_uses(&build_rules(&doc), Path::new("/tmp"));
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
        assert_eq!(
            matching_uses(&build_rules(&doc), Path::new("/tmp")),
            vec!["first"]
        );

        let doc: toml::Value = toml::from_str(
            r#"
            [open]
            rules = [{ mime = "folder/*", use = "custom" }]
            "#,
        )
        .unwrap();
        assert_eq!(
            matching_uses(&build_rules(&doc), Path::new("/tmp")),
            vec!["custom"]
        );
    }

    #[test]
    fn options_for_intersects_rules_across_targets() {
        let doc: toml::Value = toml::from_str(
            r#"
            [opener]
            a = { run = "a %s" }
            b = { run = "b %s" }
            c = { run = "c %s" }

            [open]
            rules = [
                { url = "*.txt", use = ["a", "b"] },
                { url = "*",     use = ["b", "c"] },
            ]
            "#,
        )
        .unwrap();

        let dir = std::env::temp_dir().join(format!(
            "nnnyaz-openwith-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let txt = dir.join("notes.txt");
        let bin = dir.join("blob.bin");
        std::fs::create_dir_all(&txt).unwrap();
        std::fs::create_dir_all(&bin).unwrap();

        let names = |paths: &[PathBuf]| -> Vec<String> {
            options_for(paths, Some(&doc))
                .into_iter()
                .map(|o| o.name)
                .collect()
        };
        assert_eq!(names(std::slice::from_ref(&txt)), vec!["a", "b"]);
        assert_eq!(names(&[txt.clone(), bin.clone()]), vec!["b"]);

        let _ = std::fs::remove_dir_all(&dir);
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
        assert_eq!(openers["open"].command, "nnnyaz:open");
        assert_eq!(fallback()[0].command, "nnnyaz:open");
    }
}
