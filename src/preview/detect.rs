use std::{fs, io::Read, path::Path};

#[derive(PartialEq)]
pub enum Kind {
    Text,
    Json,
    Image,
    Video,
    Pdf,
    Font,
    Archive,
    Binary,
}

const ARCHIVE_MIMES: &[&str] = &[
    "application/zip",
    "application/gzip",
    "application/x-gzip",
    "application/x-tar",
    "application/x-7z-compressed",
    "application/x-rar-compressed",
    "application/x-bzip2",
    "application/x-xz",
    "application/zstd",
    "application/x-lzma",
    "application/x-brotli",
    "application/x-cpio",
    "application/x-ar",
];

const ARCHIVE_EXTS: &[&str] = &[
    "zip", "tar", "gz", "tgz", "bz2", "xz", "7z", "rar", "zst", "lz4", "cpio", "lzma", "lz",
];

const VIDEO_EXTS: &[&str] = &[
    "mp4", "mkv", "webm", "mov", "avi", "m4v", "flv", "wmv", "mpg", "mpeg", "ts", "m2ts", "3gp",
    "ogv",
];

const FONT_EXTS: &[&str] = &[
    "ttf", "otf", "ttc", "otc", "woff", "woff2", "pfa", "pfb", "eot",
];

/// Decide how a file should be previewed: magic bytes first, then extension,
/// then NUL/control-character heuristics to tell text from binary.
pub fn detect(path: &Path) -> Kind {
    if path
        .extension()
        .map(|e| e.eq_ignore_ascii_case("json"))
        .unwrap_or(false)
    {
        return Kind::Json;
    }

    let mut buf = [0u8; 4096];
    let n = match fs::File::open(path) {
        Ok(mut f) => f.read(&mut buf).unwrap_or(0),
        Err(_) => 0,
    };
    let head = &buf[..n];

    if let Some(info) = infer::get(head) {
        let m = info.mime_type();
        if m.starts_with("image/") {
            return Kind::Image;
        }
        if m.starts_with("video/") {
            return Kind::Video;
        }
        if m == "application/pdf" {
            return Kind::Pdf;
        }
        if m.starts_with("font/")
            || matches!(
                m,
                "application/font-sfnt" | "application/font-woff" | "application/vnd.ms-fontobject"
            )
        {
            return Kind::Font;
        }
        if m.starts_with("text/") {
            return Kind::Text;
        }
        if ARCHIVE_MIMES.contains(&m) {
            return Kind::Archive;
        }
        return Kind::Binary;
    }

    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        if VIDEO_EXTS.iter().any(|x| x.eq_ignore_ascii_case(ext)) {
            return Kind::Video;
        }
        if FONT_EXTS.iter().any(|x| x.eq_ignore_ascii_case(ext)) {
            return Kind::Font;
        }
        if ARCHIVE_EXTS.iter().any(|x| x.eq_ignore_ascii_case(ext)) {
            return Kind::Archive;
        }
    }

    if head.contains(&0) {
        return Kind::Binary;
    }
    match std::str::from_utf8(head) {
        Ok(s) => {
            let controls = s
                .chars()
                .filter(|c| c.is_control() && !matches!(c, '\t' | '\n' | '\r'))
                .count();
            if controls * 10 > s.chars().count() {
                Kind::Binary
            } else {
                Kind::Text
            }
        }
        Err(e) if e.error_len().is_none() => Kind::Text,
        Err(_) => Kind::Binary,
    }
}

#[cfg(test)]
mod tests {
    use super::{detect, Kind};
    use std::fs;

    fn temp(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "nnnyaz-detect-{}-{}-{name}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::write(&dir, bytes).unwrap();
        dir
    }

    #[test]
    fn detects_fonts_by_magic_and_extension() {
        // TrueType sfnt magic wins over a non-font extension.
        let p = temp("ttf-magic.bin", &[0x00, 0x01, 0x00, 0x00, 0x00, 0, 0, 0]);
        assert!(matches!(detect(&p), Kind::Font));
        let _ = fs::remove_file(&p);

        // WOFF magic.
        let p = temp("woff-magic.bin", b"wOFF\x00\x01\x00\x00");
        assert!(matches!(detect(&p), Kind::Font));
        let _ = fs::remove_file(&p);

        // Unknown bytes with a font extension still route to the font previewer.
        let p = temp("extension.ttc", b"not really a font");
        assert!(matches!(detect(&p), Kind::Font));
        let _ = fs::remove_file(&p);
    }
}
