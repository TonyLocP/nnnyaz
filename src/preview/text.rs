use std::{fs, io::Read, path::Path, sync::LazyLock};

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use syntect::{
    easy::HighlightLines,
    highlighting::{FontStyle, Style as SynStyle},
    parsing::SyntaxSet,
    util::LinesWithEndings,
};
use two_face::theme::{EmbeddedLazyThemeSet, EmbeddedThemeName};

use crate::util::sanitize;

/// Cap on how much of a file is read for the preview.
const TEXT_LIMIT: u64 = 256 * 1024;

static SYNTAXES: LazyLock<SyntaxSet> = LazyLock::new(two_face::syntax::extra_newlines);
static THEMES: LazyLock<EmbeddedLazyThemeSet> = LazyLock::new(two_face::theme::extra);

/// Read up to `TEXT_LIMIT` bytes of a file and syntax-highlight them.
pub fn preview(path: &Path, tab_size: usize) -> std::io::Result<Vec<Line<'static>>> {
    let mut buf = Vec::new();
    fs::File::open(path)?
        .take(TEXT_LIMIT)
        .read_to_end(&mut buf)?;
    if buf.len() as u64 == TEXT_LIMIT {
        // Cut back to the last complete line so highlighting stays sane.
        if let Some(p) = buf.iter().rposition(|&b| b == b'\n') {
            buf.truncate(p + 1);
        }
    }
    if buf.is_empty() {
        return Ok(vec![Line::styled(
            "(empty file)",
            Style::new().fg(Color::DarkGray),
        )]);
    }
    let content = String::from_utf8_lossy(&buf);
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(highlight(&content, ext, &name, tab_size))
}

/// Syntax-highlight `content` as if it came from a file with the given
/// extension and name. Used for plain files and for pretty-printed JSON.
pub fn highlight(content: &str, ext: &str, name: &str, tab_size: usize) -> Vec<Line<'static>> {
    let syntax = SYNTAXES
        .find_syntax_by_extension(ext)
        .or_else(|| {
            if name.is_empty() {
                None
            } else {
                SYNTAXES.find_syntax_by_name(name)
            }
        })
        .or_else(|| {
            content
                .lines()
                .next()
                .and_then(|l| SYNTAXES.find_syntax_by_first_line(l))
        })
        .unwrap_or_else(|| SYNTAXES.find_syntax_plain_text());
    let theme = THEMES.get(EmbeddedThemeName::CatppuccinMocha);

    let mut hl = HighlightLines::new(syntax, theme);
    let mut out = Vec::new();
    for line in LinesWithEndings::from(content) {
        let line = expand_tabs(line, tab_size);
        match hl.highlight_line(&line, &SYNTAXES) {
            Ok(regions) => {
                let mut spans: Vec<Span> = Vec::new();
                for (st, text) in regions {
                    let text = sanitize(text);
                    if !text.is_empty() {
                        spans.push(Span::styled(text, convert(st)));
                    }
                }
                out.push(Line::from(spans));
            }
            Err(_) => out.push(Line::raw(sanitize(&line))),
        }
    }
    out
}

/// Expand tabs to the next multiple of `tab_size` columns. A size of zero
/// leaves tabs alone for the caller to sanitize.
fn expand_tabs(line: &str, tab_size: usize) -> String {
    if tab_size == 0 || !line.contains('\t') {
        return line.to_string();
    }
    let mut out = String::with_capacity(line.len());
    let mut col = 0usize;
    for ch in line.chars() {
        if ch == '\t' {
            let spaces = tab_size - (col % tab_size);
            for _ in 0..spaces {
                out.push(' ');
            }
            col += spaces;
        } else {
            out.push(ch);
            if ch != '\n' && ch != '\r' {
                col += 1;
            }
        }
    }
    out
}

fn convert(s: SynStyle) -> Style {
    let mut st = Style::new().fg(Color::Rgb(s.foreground.r, s.foreground.g, s.foreground.b));
    if s.font_style.contains(FontStyle::BOLD) {
        st = st.add_modifier(Modifier::BOLD);
    }
    if s.font_style.contains(FontStyle::ITALIC) {
        st = st.add_modifier(Modifier::ITALIC);
    }
    if s.font_style.contains(FontStyle::UNDERLINE) {
        st = st.add_modifier(Modifier::UNDERLINED);
    }
    st
}

#[cfg(test)]
mod tests {
    use super::{expand_tabs, highlight};

    #[test]
    fn tabs_expand_to_stops() {
        assert_eq!(expand_tabs("\tx", 4), "    x");
        assert_eq!(expand_tabs("a\tb", 4), "a   b");
        assert_eq!(expand_tabs("abcd\te", 4), "abcd    e");
        assert_eq!(expand_tabs("a\tb", 2), "a b");
        assert_eq!(expand_tabs("a\tb", 0), "a\tb");
    }

    #[test]
    fn highlighting_expands_tabs() {
        let lines = highlight("a\tb\n", "txt", "", 4);
        let text: String = lines[0]
            .spans
            .iter()
            .map(|span| span.content.to_string())
            .collect();
        assert_eq!(text, "a   b");
    }
}
