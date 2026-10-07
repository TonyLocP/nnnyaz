use std::{fs, io::Read, path::Path};

pub enum Kind {
    Text,
    Json,
    Image,
    Video,
    Pdf,
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
        if m.starts_with("text/") {
            return Kind::Text;
        }
        if ARCHIVE_MIMES.contains(&m) {
            return Kind::Archive;
        }
        return Kind::Binary;
    }

    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
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
