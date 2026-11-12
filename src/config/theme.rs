//! Semantic theme roles, parsed from `theme.toml`.

use ratatui::style::{Color, Modifier, Style};

#[derive(Clone, Copy, Debug)]
pub struct Theme {
    pub fg: Style,
    pub dim: Style,
    pub selection: Style,
    pub directory: Style,
    pub tab_active: Style,
    pub tab_inactive: Style,
    pub prefix_key: Style,
    pub prefix_desc: Style,
    pub info_section: Style,
    pub info_label: Style,
    pub status: Style,
    pub status_error: Style,
    pub status_info: Style,
    pub task_running: Style,
    pub task_done: Style,
    pub task_failed: Style,
    pub task_cancelled: Style,
    pub task_percent: Style,
    pub preview_title: Style,
    pub border: Style,
}

impl Default for Theme {
    fn default() -> Self {
        Theme {
            fg: Style::new(),
            dim: Style::new().fg(Color::DarkGray),
            selection: Style::new().add_modifier(Modifier::REVERSED),
            directory: Style::new().fg(Color::Blue).add_modifier(Modifier::BOLD),
            tab_active: Style::new().add_modifier(Modifier::REVERSED | Modifier::BOLD),
            tab_inactive: Style::new().fg(Color::DarkGray),
            prefix_key: Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            prefix_desc: Style::new().fg(Color::Gray),
            info_section: Style::new().fg(Color::Green).add_modifier(Modifier::BOLD),
            info_label: Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            status: Style::new(),
            status_error: Style::new().fg(Color::Red),
            status_info: Style::new().fg(Color::Yellow),
            task_running: Style::new().fg(Color::Yellow),
            task_done: Style::new().fg(Color::Green),
            task_failed: Style::new().fg(Color::Red),
            task_cancelled: Style::new().fg(Color::DarkGray),
            task_percent: Style::new().fg(Color::Gray),
            preview_title: Style::new(),
            border: Style::new().fg(Color::DarkGray),
        }
    }
}

impl Theme {
    /// Apply a `theme.toml` document. Accepts either a `[theme]` wrapper or
    /// roles at the top level.
    pub fn apply(&mut self, value: &toml::Value, warnings: &mut Vec<String>) {
        let Some(table) = value.as_table() else {
            warnings.push("theme.toml: expected a table".into());
            return;
        };
        let table = match table.get("theme") {
            Some(toml::Value::Table(t)) => t,
            Some(_) => {
                warnings.push("theme.toml: 'theme' must be a table".into());
                return;
            }
            None => table,
        };
        for (name, spec) in table {
            let Some(style) = parse_style(spec, name, warnings) else {
                continue;
            };
            match name.as_str() {
                "fg" => self.fg = style,
                "dim" => self.dim = style,
                "selection" => self.selection = style,
                "directory" => self.directory = style,
                "tab_active" => self.tab_active = style,
                "tab_inactive" => self.tab_inactive = style,
                "prefix_key" => self.prefix_key = style,
                "prefix_desc" => self.prefix_desc = style,
                "info_section" => self.info_section = style,
                "info_label" => self.info_label = style,
                "status" => self.status = style,
                "status_error" => self.status_error = style,
                "status_info" => self.status_info = style,
                "task_running" => self.task_running = style,
                "task_done" => self.task_done = style,
                "task_failed" => self.task_failed = style,
                "task_cancelled" => self.task_cancelled = style,
                "task_percent" => self.task_percent = style,
                "preview_title" => self.preview_title = style,
                "border" => self.border = style,
                other => warnings.push(format!("theme.toml: unknown role '{other}'")),
            }
        }
    }
}

fn parse_style(value: &toml::Value, name: &str, warnings: &mut Vec<String>) -> Option<Style> {
    match value {
        toml::Value::String(color) => match parse_color(color) {
            Ok(color) => Some(Style::new().fg(color)),
            Err(e) => {
                warnings.push(format!("theme.toml: {name}: {e}"));
                None
            }
        },
        toml::Value::Table(table) => {
            let mut style = Style::new();
            for (key, value) in table {
                match key.as_str() {
                    "fg" | "color" => match value.as_str().map(parse_color) {
                        Some(Ok(color)) => style = style.fg(color),
                        Some(Err(e)) => warnings.push(format!("theme.toml: {name}.fg: {e}")),
                        None => warnings.push(format!("theme.toml: {name}.fg must be a string")),
                    },
                    "bg" => match value.as_str().map(parse_color) {
                        Some(Ok(color)) => style = style.bg(color),
                        Some(Err(e)) => warnings.push(format!("theme.toml: {name}.bg: {e}")),
                        None => warnings.push(format!("theme.toml: {name}.bg must be a string")),
                    },
                    "bold" => style = with(style, value, Modifier::BOLD, name, key, warnings),
                    "dim" => style = with(style, value, Modifier::DIM, name, key, warnings),
                    "italic" => style = with(style, value, Modifier::ITALIC, name, key, warnings),
                    "underline" => {
                        style = with(style, value, Modifier::UNDERLINED, name, key, warnings)
                    }
                    "reversed" | "reverse" => {
                        style = with(style, value, Modifier::REVERSED, name, key, warnings)
                    }
                    other => warnings.push(format!("theme.toml: {name}: unknown key '{other}'")),
                }
            }
            Some(style)
        }
        _ => {
            warnings.push(format!(
                "theme.toml: {name} must be a color string or table"
            ));
            None
        }
    }
}

fn with(
    style: Style,
    value: &toml::Value,
    modifier: Modifier,
    role: &str,
    key: &str,
    warnings: &mut Vec<String>,
) -> Style {
    match value.as_bool() {
        Some(true) => style.add_modifier(modifier),
        Some(false) => style,
        None => {
            warnings.push(format!("theme.toml: {role}.{key} must be a boolean"));
            style
        }
    }
}

/// Parse a named ANSI color, `#rgb`/`#rrggbb`, or a 0–255 palette index.
pub fn parse_color(input: &str) -> Result<Color, String> {
    let s = input.trim();
    let lower = s.to_ascii_lowercase();
    Ok(match lower.as_str() {
        "default" | "reset" => Color::Reset,
        "black" => Color::Black,
        "red" => Color::Red,
        "green" => Color::Green,
        "yellow" => Color::Yellow,
        "blue" => Color::Blue,
        "magenta" => Color::Magenta,
        "cyan" => Color::Cyan,
        "gray" | "grey" => Color::Gray,
        "darkgray" | "darkgrey" => Color::DarkGray,
        "lightred" => Color::LightRed,
        "lightgreen" => Color::LightGreen,
        "lightyellow" => Color::LightYellow,
        "lightblue" => Color::LightBlue,
        "lightmagenta" => Color::LightMagenta,
        "lightcyan" => Color::LightCyan,
        "white" => Color::White,
        _ if lower.starts_with('#') => parse_hex(&lower)?,
        _ => match lower.parse::<u8>() {
            Ok(n) => Color::Indexed(n),
            Err(_) => return Err(format!("unknown color '{input}'")),
        },
    })
}

fn parse_hex(s: &str) -> Result<Color, String> {
    let hex = &s[1..];
    let (r, g, b) = match hex.len() {
        3 => {
            let mut it = hex.chars();
            let r = it.next().unwrap();
            let g = it.next().unwrap();
            let b = it.next().unwrap();
            (
                u8::from_str_radix(&format!("{r}{r}"), 16),
                u8::from_str_radix(&format!("{g}{g}"), 16),
                u8::from_str_radix(&format!("{b}{b}"), 16),
            )
        }
        6 => (
            u8::from_str_radix(&hex[0..2], 16),
            u8::from_str_radix(&hex[2..4], 16),
            u8::from_str_radix(&hex[4..6], 16),
        ),
        _ => return Err(format!("invalid hex color '{s}'")),
    };
    match (r, g, b) {
        (Ok(r), Ok(g), Ok(b)) => Ok(Color::Rgb(r, g, b)),
        _ => Err(format!("invalid hex color '{s}'")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_colors() {
        assert_eq!(parse_color("blue").unwrap(), Color::Blue);
        assert_eq!(parse_color("DarkGray").unwrap(), Color::DarkGray);
        assert_eq!(parse_color("#fff").unwrap(), Color::Rgb(255, 255, 255));
        assert_eq!(parse_color("#1e1e2e").unwrap(), Color::Rgb(30, 30, 46));
        assert_eq!(parse_color("42").unwrap(), Color::Indexed(42));
        assert!(parse_color("nope").is_err());
    }

    #[test]
    fn applies_roles_and_warns_on_unknown() {
        let mut theme = Theme::default();
        let mut warnings = Vec::new();
        let doc: toml::Value = toml::from_str(
            r#"
            [theme]
            directory = "red"
            selection = { reversed = true, bold = true }
            nope = "blue"
            "#,
        )
        .unwrap();
        theme.apply(&doc, &mut warnings);
        assert_eq!(theme.directory.fg, Some(Color::Red));
        assert_eq!(theme.directory.add_modifier, Modifier::empty());
        assert!(theme.selection.add_modifier.contains(Modifier::REVERSED));
        assert!(theme.selection.add_modifier.contains(Modifier::BOLD));
        assert_eq!(warnings.len(), 1);
    }
}
