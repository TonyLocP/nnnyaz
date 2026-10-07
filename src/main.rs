use std::io::{self, Write};
use std::time::{Duration, Instant};

mod app;
mod preview;
mod util;

use ratatui_image::picker::{Picker, ProtocolType};

fn main() -> io::Result<()> {
    let mut app = app::App::new()?;
    let mut terminal = ratatui::init();
    let picker = detect_picker();
    app.attach_picker(picker);
    let res = app.run(&mut terminal);
    ratatui::restore();
    res
}

fn detect_picker() -> Picker {
    let picker = if in_tmux() {
        tmux_picker()
    } else {
        Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks())
    };
    eprintln!(
        "rnn: image protocol = {:?}, font = {:?}",
        picker.protocol_type(),
        picker.font_size()
    );
    picker
}

fn in_tmux() -> bool {
    std::env::var("TERM").is_ok_and(|t| t.starts_with("tmux"))
        || std::env::var("TERM_PROGRAM").is_ok_and(|t| t == "tmux")
        || std::env::var_os("TMUX").is_some()
}

fn tmux_picker() -> Picker {
    let font = probe_cell_size().unwrap_or((10, 20));
    #[allow(deprecated)]
    let mut picker = Picker::from_fontsize(font);
    picker.set_protocol_type(outer_protocol());
    picker
}

fn outer_protocol() -> ProtocolType {
    let nonempty = |name: &str| std::env::var(name).is_ok_and(|v| !v.is_empty());

    if nonempty("KITTY_WINDOW_ID")
        || nonempty("GHOSTTY_RESOURCES_DIR")
        || nonempty("GHOSTTY_BIN_DIR")
        || std::env::var("TERM_PROGRAM").is_ok_and(|v| v.contains("ghostty"))
        || std::env::var("TERM").is_ok_and(|t| t.contains("kitty") || t.contains("ghostty"))
    {
        return ProtocolType::Kitty;
    }
    if nonempty("ITERM_SESSION_ID") || nonempty("WEZTERM_EXECUTABLE") {
        return ProtocolType::Iterm2;
    }
    if std::env::var("TERM_PROGRAM").is_ok_and(|v| {
        [
            "iTerm",
            "WezTerm",
            "mintty",
            "vscode",
            "Tabby",
            "Hyper",
            "rio",
            "WarpTerminal",
        ]
        .iter()
        .any(|p| v.contains(p))
    }) || std::env::var("LC_TERMINAL").is_ok_and(|v| v.contains("iTerm"))
    {
        return ProtocolType::Iterm2;
    }
    ProtocolType::Halfblocks
}

fn probe_cell_size() -> Option<(u16, u16)> {
    use rustix::event::{poll, PollFd, PollFlags};
    use rustix::io::read;

    io::stdout().write_all(b"\x1b[16t\x1b[5n").ok()?;
    io::stdout().flush().ok()?;

    let stdin = io::stdin();
    let mut buf: Vec<u8> = Vec::new();
    let deadline = Instant::now() + Duration::from_millis(700);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        let mut fds = [PollFd::new(&stdin, PollFlags::IN)];
        match poll(&mut fds, remaining.as_millis() as i32) {
            Ok(0) => break,
            Ok(_) => {}
            Err(rustix::io::Errno::INTR) => continue,
            Err(_) => break,
        }
        let mut chunk = [0u8; 256];
        match read(&stdin, &mut chunk) {
            Ok(0) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(rustix::io::Errno::INTR) => continue,
            Err(_) => break,
        }
        if buf.windows(4).any(|w| w == b"\x1b[0n") {
            break;
        }
    }
    parse_cell_size(&buf)
}

fn parse_cell_size(buf: &[u8]) -> Option<(u16, u16)> {
    let pat = b"\x1b[6;";
    let start = buf.windows(pat.len()).position(|w| w == pat)? + pat.len();
    let rest = &buf[start..];
    let end = rest
        .iter()
        .take_while(|&&b| b != b't' && b != b'R' && b != b'c' && b != b'n')
        .count();
    if end == rest.len() || rest[end] != b't' {
        return None;
    }
    let mut parts = std::str::from_utf8(&rest[..end]).ok()?.split(';');
    let h: u16 = parts.next()?.parse().ok()?;
    let w: u16 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || w == 0 || h == 0 {
        return None;
    }
    Some((w, h))
}
