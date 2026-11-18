//! Declarative keymaps: key-sequence parsing, a trie, and Yazi-style TOML.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::action::Action;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Mods {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ChordCode {
    Char(char),
    Enter,
    Esc,
    Tab,
    Backspace,
    Delete,
    Insert,
    Home,
    End,
    PageUp,
    PageDown,
    Up,
    Down,
    Left,
    Right,
    F(u8),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct KeyChord {
    pub code: ChordCode,
    pub mods: Mods,
}

impl KeyChord {
    pub fn matches(&self, ev: &KeyEvent) -> bool {
        let ctrl = ev.modifiers.contains(KeyModifiers::CONTROL);
        let alt = ev.modifiers.contains(KeyModifiers::ALT);
        if ctrl != self.mods.ctrl || alt != self.mods.alt {
            return false;
        }
        match self.code {
            ChordCode::Char(c) => matches!(ev.code, KeyCode::Char(ec) if ec == c),
            code => {
                let shift = ev.modifiers.contains(KeyModifiers::SHIFT);
                shift == self.mods.shift && code_matches(code, ev.code)
            }
        }
    }

    /// Compact label for the which-key menu and status line.
    pub fn label(&self) -> String {
        let key = match self.code {
            ChordCode::Char(' ') => "Space".to_string(),
            ChordCode::Char(c) => c.to_string(),
            ChordCode::Enter => "Enter".into(),
            ChordCode::Esc => "Esc".into(),
            ChordCode::Tab => "Tab".into(),
            ChordCode::Backspace => "Bksp".into(),
            ChordCode::Delete => "Del".into(),
            ChordCode::Insert => "Ins".into(),
            ChordCode::Home => "Home".into(),
            ChordCode::End => "End".into(),
            ChordCode::PageUp => "PgUp".into(),
            ChordCode::PageDown => "PgDn".into(),
            ChordCode::Up => "↑".into(),
            ChordCode::Down => "↓".into(),
            ChordCode::Left => "←".into(),
            ChordCode::Right => "→".into(),
            ChordCode::F(n) => format!("F{n}"),
        };
        if self.mods.ctrl || self.mods.alt || self.mods.shift {
            let mut s = String::from("<");
            if self.mods.ctrl {
                s.push_str("C-");
            }
            if self.mods.alt {
                s.push_str("A-");
            }
            if self.mods.shift {
                s.push_str("S-");
            }
            s.push_str(&key);
            s.push('>');
            s
        } else {
            key
        }
    }
}

fn code_matches(code: ChordCode, ev: KeyCode) -> bool {
    match code {
        ChordCode::Char(c) => matches!(ev, KeyCode::Char(ec) if ec == c),
        ChordCode::Enter => ev == KeyCode::Enter,
        ChordCode::Esc => ev == KeyCode::Esc,
        ChordCode::Tab => ev == KeyCode::Tab,
        ChordCode::Backspace => ev == KeyCode::Backspace,
        ChordCode::Delete => ev == KeyCode::Delete,
        ChordCode::Insert => ev == KeyCode::Insert,
        ChordCode::Home => ev == KeyCode::Home,
        ChordCode::End => ev == KeyCode::End,
        ChordCode::PageUp => ev == KeyCode::PageUp,
        ChordCode::PageDown => ev == KeyCode::PageDown,
        ChordCode::Up => ev == KeyCode::Up,
        ChordCode::Down => ev == KeyCode::Down,
        ChordCode::Left => ev == KeyCode::Left,
        ChordCode::Right => ev == KeyCode::Right,
        ChordCode::F(n) => ev == KeyCode::F(n),
    }
}

/// Parse a whitespace-separated key sequence such as `g g` or `<C-d>`.
pub fn parse_seq(input: &str) -> Result<Vec<KeyChord>, String> {
    let mut out = Vec::new();
    for token in input.split_whitespace() {
        out.push(parse_chord(token)?);
    }
    if out.is_empty() {
        return Err("empty key sequence".into());
    }
    Ok(out)
}

fn parse_chord(token: &str) -> Result<KeyChord, String> {
    if token.starts_with('<') && token.ends_with('>') && token.len() > 2 {
        let inner = &token[1..token.len() - 1];
        parse_angle(inner).ok_or_else(|| format!("invalid key '{token}'"))
    } else {
        let mut chars = token.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) => Ok(KeyChord {
                code: ChordCode::Char(c),
                mods: Mods::default(),
            }),
            _ => Err(format!("invalid key '{token}' (use <Space> for space)")),
        }
    }
}

fn parse_angle(inner: &str) -> Option<KeyChord> {
    let parts: Vec<&str> = inner.split('-').collect();
    let mut mods = Mods::default();
    let mut i = 0;
    while i + 1 < parts.len() {
        match parts[i].to_ascii_lowercase().as_str() {
            "c" | "ctrl" | "control" => mods.ctrl = true,
            "a" | "alt" | "m" | "meta" => mods.alt = true,
            "s" | "shift" => mods.shift = true,
            _ => break,
        }
        i += 1;
    }
    let key = parts[i..].join("-");
    if key.is_empty() {
        return None;
    }
    let code = parse_code(&key)?;
    // A shifted character is already encoded in the character itself, which is
    // how terminals report it (`Shift+g` arrives as `G`).
    if mods.shift {
        if let ChordCode::Char(c) = code {
            if c.is_alphabetic() {
                return Some(KeyChord {
                    code: ChordCode::Char(c.to_uppercase().next().unwrap_or(c)),
                    mods: Mods {
                        shift: false,
                        ..mods
                    },
                });
            }
        }
    }
    Some(KeyChord { code, mods })
}

fn parse_code(key: &str) -> Option<ChordCode> {
    let lower = key.to_ascii_lowercase();
    Some(match lower.as_str() {
        "space" => ChordCode::Char(' '),
        "enter" | "return" => ChordCode::Enter,
        "esc" | "escape" => ChordCode::Esc,
        "tab" => ChordCode::Tab,
        "backspace" | "bs" => ChordCode::Backspace,
        "delete" | "del" => ChordCode::Delete,
        "insert" | "ins" => ChordCode::Insert,
        "home" => ChordCode::Home,
        "end" => ChordCode::End,
        "pageup" | "pgup" => ChordCode::PageUp,
        "pagedown" | "pgdn" | "pagedn" => ChordCode::PageDown,
        "up" => ChordCode::Up,
        "down" => ChordCode::Down,
        "left" => ChordCode::Left,
        "right" => ChordCode::Right,
        _ => {
            if let Some(n) = lower.strip_prefix('f') {
                if let Ok(n) = n.parse::<u8>() {
                    if (1..=24).contains(&n) {
                        return Some(ChordCode::F(n));
                    }
                }
            }
            let mut chars = key.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => ChordCode::Char(c),
                _ => return None,
            }
        }
    })
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Context {
    Mgr,
    Tasks,
    Info,
}

impl Context {
    pub fn name(self) -> &'static str {
        match self {
            Context::Mgr => "mgr",
            Context::Tasks => "tasks",
            Context::Info => "info",
        }
    }
}

#[derive(Clone, Default, Debug)]
pub struct Node {
    action: Option<Action>,
    desc: Option<String>,
    children: Vec<(KeyChord, Node)>,
}

impl Node {
    pub fn action(&self) -> Option<&Action> {
        self.action.as_ref()
    }

    pub fn desc(&self) -> Option<&str> {
        self.desc.as_deref()
    }

    pub fn has_children(&self) -> bool {
        !self.children.is_empty()
    }

    pub fn children(&self) -> &[(KeyChord, Node)] {
        &self.children
    }

    pub fn child(&self, ev: &KeyEvent) -> Option<(&KeyChord, &Node)> {
        self.children
            .iter()
            .find(|(chord, _)| chord.matches(ev))
            .map(|(chord, node)| (chord, node))
    }

    pub fn walk(&self, seq: &[KeyChord]) -> Option<&Node> {
        let mut node = self;
        for chord in seq {
            node = node
                .children
                .iter()
                .find(|(c, _)| c == chord)
                .map(|(_, n)| n)?;
        }
        Some(node)
    }

    fn child_mut(&mut self, chord: &KeyChord) -> &mut Node {
        if let Some(i) = self.children.iter().position(|(c, _)| c == chord) {
            return &mut self.children[i].1;
        }
        self.children.push((*chord, Node::default()));
        let i = self.children.len() - 1;
        &mut self.children[i].1
    }

    pub fn insert(&mut self, seq: &[KeyChord], action: Action, desc: Option<String>) {
        let Some((last, rest)) = seq.split_last() else {
            return;
        };
        let mut node = self;
        for chord in rest {
            node = node.child_mut(chord);
        }
        let node = node.child_mut(last);
        node.action = Some(action);
        node.desc = desc;
    }

    pub fn clear(&mut self) {
        *self = Node::default();
    }
}

#[derive(Clone, Debug)]
pub struct Keymap {
    pub mgr: Node,
    pub tasks: Node,
    pub info: Node,
}

impl Keymap {
    pub fn node(&self, ctx: Context) -> &Node {
        match ctx {
            Context::Mgr => &self.mgr,
            Context::Tasks => &self.tasks,
            Context::Info => &self.info,
        }
    }

    fn node_mut(&mut self, ctx: Context) -> &mut Node {
        match ctx {
            Context::Mgr => &mut self.mgr,
            Context::Tasks => &mut self.tasks,
            Context::Info => &mut self.info,
        }
    }

    pub fn insert(&mut self, ctx: Context, seq: &str, action: &str, desc: Option<&str>) {
        let Ok(seq) = parse_seq(seq) else {
            return;
        };
        let Ok(action) = Action::parse(action) else {
            return;
        };
        self.node_mut(ctx)
            .insert(&seq, action, desc.map(str::to_string));
    }

    /// Insert a binding from a programmatic source (`init.lua`), reporting
    /// parse errors as warnings.
    pub fn set_binding(
        &mut self,
        ctx: Context,
        on: &str,
        run: &str,
        desc: Option<String>,
        warnings: &mut Vec<String>,
    ) {
        self.apply_def(ctx, on, &[run.to_string()], desc, warnings);
    }

    fn replace(&mut self, ctx: Context) {
        self.node_mut(ctx).clear();
    }

    /// Apply a `keymap.toml` document.
    pub fn apply(&mut self, value: &toml::Value, warnings: &mut Vec<String>) {
        let Some(table) = value.as_table() else {
            warnings.push("keymap.toml: expected a table".into());
            return;
        };
        for (name, section) in table {
            let ctx = match name.as_str() {
                "mgr" => Context::Mgr,
                "tasks" => Context::Tasks,
                "info" | "spot" => Context::Info,
                // Yazi contexts that rnn does not keymap yet.
                "input" | "confirm" | "pick" | "cmp" | "help" | "which" => continue,
                other => {
                    warnings.push(format!("keymap.toml: unknown context '{other}'"));
                    continue;
                }
            };
            let Some(section) = section.as_table() else {
                warnings.push(format!("keymap.toml: [{name}] must be a table"));
                continue;
            };
            if let Some(v) = section.get("keymap") {
                let defs = parse_defs(v, name, warnings);
                self.replace(ctx);
                for (on, runs, desc) in defs {
                    self.apply_def(ctx, &on, &runs, desc, warnings);
                }
            } else {
                for key in ["prepend_keymap", "append_keymap"] {
                    if let Some(v) = section.get(key) {
                        for (on, runs, desc) in parse_defs(v, name, warnings) {
                            self.apply_def(ctx, &on, &runs, desc, warnings);
                        }
                    }
                }
            }
            for key in section.keys() {
                if !matches!(key.as_str(), "keymap" | "prepend_keymap" | "append_keymap") {
                    warnings.push(format!("keymap.toml: [{name}] unknown key '{key}'"));
                }
            }
        }
    }

    fn apply_def(
        &mut self,
        ctx: Context,
        on: &str,
        runs: &[String],
        desc: Option<String>,
        warnings: &mut Vec<String>,
    ) {
        let seq = match parse_seq(on) {
            Ok(seq) => seq,
            Err(e) => {
                warnings.push(format!("keymap.toml: [{}] {e}", ctx.name()));
                return;
            }
        };
        let mut actions = Vec::new();
        for run in runs {
            match Action::parse(run) {
                Ok(action) => actions.push(action),
                Err(e) => {
                    warnings.push(format!("keymap.toml: [{}] {e}", ctx.name()));
                    return;
                }
            }
        }
        if actions.is_empty() {
            return;
        }
        let action = if actions.len() == 1 {
            actions.pop().unwrap()
        } else {
            Action::Chain(actions)
        };
        self.node_mut(ctx).insert(&seq, action, desc);
    }

    /// Names of custom actions referenced by user bindings.
    pub fn custom_actions(&self) -> Vec<String> {
        fn collect(action: &Action, out: &mut Vec<String>) {
            match action {
                Action::Custom(name) => out.push(name.clone()),
                Action::Chain(actions) => {
                    for action in actions {
                        collect(action, out);
                    }
                }
                _ => {}
            }
        }
        fn walk(node: &Node, out: &mut Vec<String>) {
            if let Some(action) = node.action() {
                collect(action, out);
            }
            for (_, child) in node.children() {
                walk(child, out);
            }
        }
        let mut out = Vec::new();
        walk(&self.mgr, &mut out);
        walk(&self.tasks, &mut out);
        walk(&self.info, &mut out);
        out.sort();
        out.dedup();
        out
    }
}

/// A Yazi `on` value: either a key sequence string or an array of keys.
fn parse_on(value: &toml::Value) -> Option<String> {
    match value {
        toml::Value::String(on) => Some(on.clone()),
        toml::Value::Array(items) => {
            let mut parts = Vec::with_capacity(items.len());
            for item in items {
                parts.push(item.as_str()?.to_string());
            }
            (!parts.is_empty()).then(|| parts.join(" "))
        }
        _ => None,
    }
}

/// A Yazi `run` value: either one command string or an array of them.
fn parse_runs(value: &toml::Value) -> Option<Vec<String>> {
    match value {
        toml::Value::String(run) => Some(vec![run.clone()]),
        toml::Value::Array(items) => {
            let mut runs = Vec::with_capacity(items.len());
            for item in items {
                runs.push(item.as_str()?.to_string());
            }
            (!runs.is_empty()).then_some(runs)
        }
        _ => None,
    }
}

fn parse_defs(
    value: &toml::Value,
    context: &str,
    warnings: &mut Vec<String>,
) -> Vec<(String, Vec<String>, Option<String>)> {
    let mut out = Vec::new();
    match value {
        toml::Value::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                let Some(table) = item.as_table() else {
                    warnings.push(format!(
                        "keymap.toml: [{context}] binding {} must be a table",
                        i + 1
                    ));
                    continue;
                };
                let on = table.get("on").and_then(parse_on);
                let runs = table.get("run").and_then(parse_runs);
                let (Some(on), Some(runs)) = (on, runs) else {
                    warnings.push(format!(
                        "keymap.toml: [{context}] binding {} needs 'on' and 'run'",
                        i + 1
                    ));
                    continue;
                };
                out.push((
                    on.to_string(),
                    runs,
                    table
                        .get("desc")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                ));
            }
        }
        toml::Value::Table(map) => {
            for (on, spec) in map {
                match spec {
                    toml::Value::String(run) => {
                        out.push((on.clone(), vec![run.clone()], None));
                    }
                    toml::Value::Table(table) => {
                        let Some(runs) = table.get("run").and_then(parse_runs) else {
                            warnings.push(format!(
                                "keymap.toml: [{context}] '{on}' needs a 'run' string or array"
                            ));
                            continue;
                        };
                        out.push((
                            on.clone(),
                            runs,
                            table
                                .get("desc")
                                .and_then(|v| v.as_str())
                                .map(str::to_string),
                        ));
                    }
                    _ => warnings.push(format!(
                        "keymap.toml: [{context}] '{on}' must be a string or table"
                    )),
                }
            }
        }
        _ => warnings.push(format!(
            "keymap.toml: [{context}] keymap must be an array or table"
        )),
    }
    out
}

impl Default for Keymap {
    fn default() -> Self {
        let mut km = Keymap {
            mgr: Node::default(),
            tasks: Node::default(),
            info: Node::default(),
        };
        use Context::*;

        let mgr: &[(&str, &str, Option<&str>)] = &[
            ("<C-[>", "escape", None),
            ("<C-c>", "quit_or_close", None),
            ("<C-d>", "half_page 1", None),
            ("<C-u>", "half_page -1", None),
            ("<C-b>", "page -1", None),
            ("<C-f>", "page 1", None),
            ("<C-a>", "select_all", None),
            ("<C-r>", "invert_selection", None),
            ("<C-n>", "create", None),
            ("<C-y>", "clipboard_copy", None),
            ("<C-p>", "clipboard_paste", None),
            ("<C-s>", "cancel_search", None),
            ("<C-->", "hardlink", None),
            ("<C-_>", "hardlink", None),
            ("<C-7>", "hardlink", None),
            ("<C-z>", "suspend", None),
            ("q", "quit", None),
            ("Q", "quit_cwd", Some("quit and cd")),
            ("j", "move 1", None),
            ("<Down>", "move 1", None),
            ("k", "move -1", None),
            ("<Up>", "move -1", None),
            ("J", "seek 5", None),
            ("K", "seek -5", None),
            ("<PageDown>", "page 1", None),
            ("<S-PageDown>", "half_page 1", None),
            ("<PageUp>", "page -1", None),
            ("<S-PageUp>", "half_page -1", None),
            ("<Home>", "top", None),
            ("G", "bottom", None),
            ("<End>", "bottom", None),
            ("h", "parent", None),
            ("<Left>", "parent", None),
            ("<Backspace>", "parent", None),
            ("<S-Enter>", "open_with", None),
            ("l", "enter", None),
            ("<Right>", "enter", None),
            ("<Enter>", "open", None),
            ("o", "open", None),
            ("O", "open_with", None),
            ("H", "history -1", None),
            ("L", "history 1", None),
            ("~", "help", None),
            ("<F1>", "help", None),
            ("e", "editor", None),
            ("i", "pager", None),
            ("<Space>", "toggle_select", None),
            ("v", "visual_select", None),
            ("V", "visual_unset", None),
            ("y", "yank", None),
            ("x", "cut", None),
            ("p", "paste", None),
            ("P", "paste --force", None),
            ("Y", "unyank", None),
            ("X", "unyank", None),
            ("d", "trash", None),
            ("D", "delete", None),
            ("b", "bookmark", Some("bookmarks")),
            ("B", "add_bookmark", Some("bookmark this dir")),
            ("'", "mark_jump", Some("jump to mark")),
            ("r", "rename", None),
            ("a", "create", None),
            ("A", "bulk_create", None),
            ("f", "filter", None),
            ("/", "find", None),
            ("?", "find backward", None),
            ("n", "find_next", None),
            ("N", "find_next backward", None),
            ("s", "search fd", None),
            ("S", "search rg", None),
            ("w", "tasks", None),
            ("<Tab>", "info", None),
            ("F", "spot", Some("full-screen preview")),
            ("1", "switch_tab 1", None),
            ("2", "switch_tab 2", None),
            ("3", "switch_tab 3", None),
            ("4", "switch_tab 4", None),
            ("5", "switch_tab 5", None),
            ("6", "switch_tab 6", None),
            ("7", "switch_tab 7", None),
            ("8", "switch_tab 8", None),
            ("9", "switch_tab 9", None),
            ("[", "next_tab -1", None),
            ("]", "next_tab 1", None),
            ("{", "swap_tab -1", None),
            ("}", "swap_tab 1", None),
            ("z", "fzf", None),
            ("Z", "zoxide", Some("jump with zoxide")),
            (";", "shell", None),
            (":", "shell --block", Some("run shell command (blocking)")),
            ("+", "zoom in", None),
            ("<A-->", "link absolute", None),
            ("-", "zoom out", None),
            ("_", "link relative", None),
            ("0", "zoom reset", None),
            ("<Esc>", "escape", None),
            (".", "hidden", None),
            ("T", "toggle_preview", None),
            // prefixes
            ("g", "nop", None),
            ("g g", "top", Some("top of list")),
            ("g h", "goto ~", Some("home")),
            ("g c", "goto ~/.config", Some("~/.config")),
            ("g d", "goto ~/Downloads", Some("~/Downloads")),
            (
                "g o",
                "goto ~/.local/share/osu-wine/osu!",
                Some("osu! (osu-wine)"),
            ),
            ("g p", "goto ~/Pictures", Some("~/Pictures")),
            ("g v", "goto ~/Videos", Some("~/Videos")),
            ("g <Space>", "cd", Some("go to path…")),
            ("g f", "follow", Some("follow symlink")),
            ("g t", "goto_trash", Some("trash bin")),
            (",", "nop", None),
            (", a", "sort name", Some("name ↑")),
            (", A", "sort name --reverse", Some("name ↓")),
            (", n", "sort natural", Some("natural ↑")),
            (", N", "sort natural --reverse", Some("natural ↓")),
            (", s", "sort size", Some("size ↑")),
            (", S", "sort size --reverse", Some("size ↓")),
            (", m", "sort mtime", Some("modified ↑")),
            (", M", "sort mtime --reverse", Some("modified ↓")),
            (", b", "sort btime", Some("created ↑")),
            (", B", "sort btime --reverse", Some("created ↓")),
            (", e", "sort ext", Some("extension ↑")),
            (", E", "sort ext --reverse", Some("extension ↓")),
            (", r", "sort random", Some("random")),
            ("c", "nop", None),
            ("c c", "copy path", Some("path")),
            ("c C", "copy url", Some("url")),
            ("c d", "copy dir_path", Some("directory path")),
            ("c D", "copy dir_url", Some("directory url")),
            ("c f", "copy filename", Some("filename")),
            ("c n", "copy stem", Some("name without extension")),
            ("m", "mark_set", Some("set mark")),
            ("m s", "linemode size", Some("size")),
            ("m p", "linemode permissions", Some("permissions")),
            ("m b", "linemode btime", Some("created")),
            ("m m", "linemode mtime", Some("modified")),
            ("m o", "linemode owner", Some("owner")),
            ("m n", "linemode none", Some("none")),
            ("t", "nop", None),
            ("t t", "new_tab here", Some("new tab here")),
            ("t n", "new_tab home", Some("new tab at home")),
            ("t r", "rename_tab", Some("rename tab")),
            ("t c", "close_tab", Some("close tab")),
            ("t [", "next_tab -1", Some("previous tab")),
            ("t ]", "next_tab 1", Some("next tab")),
        ];
        for (on, run, desc) in mgr {
            km.insert(Mgr, on, run, *desc);
        }

        // Yazi's sort keys also switch to the matching linemode, so the
        // column being sorted on is visible without a second keystroke.
        let sort_linemode: &[(&str, &str, &str)] = &[
            (", m", "sort mtime", "mtime"),
            (", M", "sort mtime --reverse", "mtime"),
            (", b", "sort btime", "btime"),
            (", B", "sort btime --reverse", "btime"),
            (", s", "sort size", "size"),
            (", S", "sort size --reverse", "size"),
        ];
        for (on, run, mode) in sort_linemode {
            let seq = parse_seq(on).expect("default sort key parses");
            let actions = vec![
                Action::parse(run).expect("default sort action parses"),
                Action::parse(&format!("linemode {mode}")).expect("default linemode parses"),
            ];
            km.mgr.insert(&seq, Action::Chain(actions), None);
        }

        let tasks: &[(&str, &str)] = &[
            ("<C-c>", "close"),
            ("<Esc>", "close"),
            ("w", "close"),
            ("q", "close"),
            ("j", "task_move 1"),
            ("<Down>", "task_move 1"),
            ("k", "task_move -1"),
            ("<Up>", "task_move -1"),
            ("g", "task_top"),
            ("<Home>", "task_top"),
            ("G", "task_bottom"),
            ("<End>", "task_bottom"),
            ("<Enter>", "task_inspect"),
            ("x", "task_cancel"),
            ("~", "help"),
            ("<F1>", "help"),
        ];
        for (on, run) in tasks {
            km.insert(Tasks, on, run, None);
        }

        let info: &[(&str, &str, Option<&str>)] = &[
            ("<C-c>", "close", None),
            ("<Esc>", "close", None),
            ("<Tab>", "close", None),
            ("~", "help", None),
            ("<F1>", "help", None),
            ("j", "info_move 1", None),
            ("<Down>", "info_move 1", None),
            ("k", "info_move -1", None),
            ("<Up>", "info_move -1", None),
            ("h", "info_swipe -1", None),
            ("<Left>", "info_swipe -1", None),
            ("l", "info_swipe 1", None),
            ("<Right>", "info_swipe 1", None),
            ("c", "nop", None),
            ("c c", "info_copy_cell", Some("copy cell")),
        ];
        for (on, run, desc) in info {
            km.insert(Info, on, run, *desc);
        }

        km
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chord(s: &str) -> KeyChord {
        parse_seq(s).unwrap()[0]
    }

    #[test]
    fn parses_sequences_and_special_keys() {
        assert_eq!(parse_seq("g g").unwrap().len(), 2);
        assert_eq!(chord("<C-d>").code, ChordCode::Char('d'));
        assert!(chord("<C-d>").mods.ctrl);
        assert_eq!(chord("<Space>").code, ChordCode::Char(' '));
        assert_eq!(chord("<S-Enter>").code, ChordCode::Enter);
        assert!(chord("<S-Enter>").mods.shift);
        assert_eq!(chord("<A-->").code, ChordCode::Char('-'));
        assert!(chord("<A-->").mods.alt);
        assert_eq!(chord("<C-->").code, ChordCode::Char('-'));
        assert!(chord("<C-->").mods.ctrl);
    }

    #[test]
    fn shifted_letters_are_normalized() {
        assert_eq!(chord("<S-g>").code, ChordCode::Char('G'));
        assert!(!chord("<S-g>").mods.shift);
    }

    #[test]
    fn rejects_bad_keys() {
        assert!(parse_seq("").is_err());
        assert!(parse_seq("<NotAKey>").is_err());
        assert!(parse_seq("ab").is_err());
    }

    #[test]
    fn default_keymap_has_core_bindings() {
        let km = Keymap::default();
        let node = km.mgr.walk(&parse_seq("g g").unwrap()).unwrap();
        assert_eq!(node.action(), Some(&Action::Top));
        let node = km.mgr.walk(&parse_seq("<C-d>").unwrap()).unwrap();
        assert_eq!(node.action(), Some(&Action::HalfPage(1)));
        assert_eq!(
            km.mgr.walk(&parse_seq("q").unwrap()).unwrap().action(),
            Some(&Action::Quit)
        );
        assert_eq!(
            km.mgr.walk(&parse_seq("Q").unwrap()).unwrap().action(),
            Some(&Action::QuitCwd)
        );
        assert_eq!(
            km.mgr.walk(&parse_seq("Z").unwrap()).unwrap().action(),
            Some(&Action::Zoxide)
        );
        assert_eq!(
            km.mgr.walk(&parse_seq("F").unwrap()).unwrap().action(),
            Some(&Action::Spot)
        );
        assert_eq!(
            km.mgr.walk(&parse_seq("<Tab>").unwrap()).unwrap().action(),
            Some(&Action::Info)
        );
        assert!(km
            .mgr
            .walk(&parse_seq("g").unwrap())
            .unwrap()
            .has_children());
    }

    #[test]
    fn default_keymap_has_marks_and_bookmarks() {
        let km = Keymap::default();
        let action = |seq: &str| {
            km.mgr
                .walk(&parse_seq(seq).unwrap())
                .unwrap()
                .action()
                .cloned()
        };
        assert_eq!(action("m"), Some(Action::MarkSet));
        assert_eq!(
            action("m s"),
            Some(Action::SetLinemode(crate::app::Linemode::Size))
        );
        assert_eq!(action("'"), Some(Action::MarkJump));
        assert_eq!(action("b"), Some(Action::Bookmark));
        assert_eq!(action("B"), Some(Action::AddBookmark));
        assert!(km
            .mgr
            .walk(&parse_seq("m").unwrap())
            .unwrap()
            .has_children());
    }

    #[test]
    fn sort_keys_also_set_the_matching_linemode() {
        let km = Keymap::default();
        let action = |seq: &str| {
            km.mgr
                .walk(&parse_seq(seq).unwrap())
                .unwrap()
                .action()
                .cloned()
        };
        assert_eq!(
            action(", m"),
            Some(Action::Chain(vec![
                Action::parse("sort mtime").unwrap(),
                Action::SetLinemode(crate::app::Linemode::Mtime),
            ]))
        );
        assert_eq!(
            action(", S"),
            Some(Action::Chain(vec![
                Action::parse("sort size --reverse").unwrap(),
                Action::SetLinemode(crate::app::Linemode::Size),
            ]))
        );
        assert_eq!(
            action(", e"),
            Some(Action::parse("sort ext").unwrap()),
            "extension sort should not change the linemode"
        );
    }

    #[test]
    fn keymap_toml_overrides_and_replaces() {
        let mut km = Keymap::default();
        let mut warnings = Vec::new();
        let doc: toml::Value = toml::from_str(
            r#"
            [mgr]
            prepend_keymap = [ { on = "d", run = "delete" } ]
            "#,
        )
        .unwrap();
        km.apply(&doc, &mut warnings);
        assert!(warnings.is_empty(), "{warnings:?}");
        let node = km.mgr.walk(&parse_seq("d").unwrap()).unwrap();
        assert_eq!(node.action(), Some(&Action::Delete));

        let doc: toml::Value = toml::from_str(
            r#"
            [mgr]
            keymap = [ { on = "x", run = "quit" } ]
            "#,
        )
        .unwrap();
        km.apply(&doc, &mut warnings);
        assert_eq!(
            km.mgr.walk(&parse_seq("x").unwrap()).unwrap().action(),
            Some(&Action::Quit)
        );
        assert!(km.mgr.walk(&parse_seq("d").unwrap()).is_none());
    }

    #[test]
    fn keymap_accepts_yazi_action_arrays() {
        let mut km = Keymap::default();
        let mut warnings = Vec::new();
        let doc: toml::Value = toml::from_str(
            r#"
            [mgr]
            prepend_keymap = [
              { on = "<Space>", run = [ "toggle", "arrow 1" ] },
              { on = [ "g", "g" ], run = "arrow top" },
              { on = "X", run = [ "remove --permanently" ] },
            ]
            "#,
        )
        .unwrap();
        km.apply(&doc, &mut warnings);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(
            km.mgr
                .walk(&parse_seq("<Space>").unwrap())
                .unwrap()
                .action(),
            Some(&Action::Chain(vec![Action::Toggle, Action::Move(1)]))
        );
        assert_eq!(
            km.mgr.walk(&parse_seq("g g").unwrap()).unwrap().action(),
            Some(&Action::Top)
        );
        assert_eq!(
            km.mgr.walk(&parse_seq("X").unwrap()).unwrap().action(),
            Some(&Action::Delete)
        );
    }

    #[test]
    fn ignores_unsupported_yazi_contexts() {
        let mut km = Keymap::default();
        let mut warnings = Vec::new();
        let doc: toml::Value = toml::from_str(
            r#"
            [input]
            prepend_keymap = [ { on = "x", run = "quit" } ]

            [mgr]
            prepend_keymap = [ { on = "x", run = "delete" } ]
            "#,
        )
        .unwrap();
        km.apply(&doc, &mut warnings);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(
            km.mgr.walk(&parse_seq("x").unwrap()).unwrap().action(),
            Some(&Action::Delete)
        );
    }

    #[test]
    fn shorthand_keymap_table() {
        let mut km = Keymap::default();
        let mut warnings = Vec::new();
        let doc: toml::Value = toml::from_str(
            r#"
            [mgr.keymap]
            "g g" = "top"
            "#,
        )
        .unwrap();
        km.apply(&doc, &mut warnings);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(
            km.mgr.walk(&parse_seq("g g").unwrap()).unwrap().action(),
            Some(&Action::Top)
        );
    }
}
