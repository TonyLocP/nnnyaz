pub fn human(n: u64) -> String {
    const U: [&str; 5] = ["B", "K", "M", "G", "T"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n}B")
    } else {
        format!("{v:.1}{}", U[i])
    }
}

/// Make a string safe for terminal rendering: strips CR/LF, turns tabs into
/// spaces and other control characters into a dot.
pub fn sanitize(s: &str) -> String {
    s.chars()
        .filter_map(|c| match c {
            '\n' | '\r' => None,
            '\t' => Some(' '),
            c if c.is_control() => Some('·'),
            c => Some(c),
        })
        .collect()
}
