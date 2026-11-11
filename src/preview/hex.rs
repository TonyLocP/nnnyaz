use std::{io::Read, path::Path};

use ratatui::{
    style::{Color, Style},
    text::{Line, Span},
};

use crate::util::human;

const DUMP_LIMIT: u64 = 64 * 1024;

pub fn preview(path: &Path) -> std::io::Result<Vec<Line<'static>>> {
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let mut buf = Vec::new();
    std::fs::File::open(path)?
        .take(DUMP_LIMIT)
        .read_to_end(&mut buf)?;

    let mut out = vec![
        Line::styled(
            format!("binary file, {}", human(size)),
            Style::new().fg(Color::Yellow),
        ),
        Line::raw(""),
    ];
    if buf.is_empty() {
        out.push(Line::styled("(empty)", Style::new().fg(Color::DarkGray)));
        return Ok(out);
    }
    for (i, chunk) in buf.chunks(16).enumerate() {
        let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02x}")).collect();
        let asc: String = chunk
            .iter()
            .map(|&b| {
                if (0x20..0x7f).contains(&b) {
                    b as char
                } else {
                    '.'
                }
            })
            .collect();
        out.push(Line::from(vec![
            Span::styled(
                format!("{:06x}  ", i * 16),
                Style::new().fg(Color::DarkGray),
            ),
            Span::styled(
                format!("{:<47}", hex.join(" ")),
                Style::new().fg(Color::Cyan),
            ),
            Span::raw(format!("  {asc}")),
        ]));
    }
    Ok(out)
}
