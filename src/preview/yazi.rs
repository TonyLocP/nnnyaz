//! Sharing yazi's image preview cache.
//!
//! Yazi stores downscaled previews in a flat temp directory, with each file
//! named by a 128-bit xxHash of its path and `stat` signature encoded as
//! unpadded base32. When a yazi installation is detected we read from that
//! cache first and write new previews into it, so both programs share one set
//! of decoded images. If yazi cannot be found, [`cache_dir`] returns `None`
//! and rnn falls back to its own cache.
//!
//! The cache layout mirrors yazi v26.9.1 (`FileSig` + `ChaSig` + `skip`). A
//! different yazi version that changes the key simply produces misses, which
//! are harmless: rnn decodes the source again and writes a fresh entry.

use std::{
    env, fs,
    hash::{Hash, Hasher},
    io,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU32, Ordering},
        OnceLock,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use twox_hash::XxHash3_128;

/// Directory yazi uses for previews, when a yazi install appears to be
/// present. `RNN_CACHE_DIR` or a configured rnn cache forces rnn's own cache.
pub fn cache_dir() -> Option<PathBuf> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(resolve_cache_dir).clone()
}

static CONFIG_CACHE_DIR: OnceLock<Option<PathBuf>> = OnceLock::new();

/// Record `preview.cache_dir` from rnn's config. Called once at startup;
/// `RNN_CACHE_DIR` still wins, and a configured directory disables sharing
/// yazi's cache (it is rnn's own cache).
pub fn set_cache_dir(dir: Option<PathBuf>) {
    let _ = CONFIG_CACHE_DIR.set(dir);
}

/// The configured rnn preview cache directory, if any.
pub fn configured_cache_dir() -> Option<PathBuf> {
    CONFIG_CACHE_DIR.get().cloned().flatten()
}

fn resolve_cache_dir() -> Option<PathBuf> {
    if env::var_os("RNN_CACHE_DIR").is_some() || configured_cache_dir().is_some() {
        return None;
    }

    let yazi_home = env::var_os("YAZI_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute());
    let config_dir = yazi_home.clone().or_else(default_config_dir);
    let cache = config_dir
        .as_deref()
        .and_then(cache_dir_from_config)
        .unwrap_or_else(default_temp_cache_dir);

    let available =
        yazi_home.is_some() || config_dir.as_deref().is_some_and(Path::is_dir) || cache.is_dir();
    if !available {
        return None;
    }

    fs::create_dir_all(&cache).ok()?;
    Some(cache)
}

/// Cache entry for `path`, or `None` when its metadata cannot be read.
pub fn target(cache: &Path, path: &Path) -> Option<PathBuf> {
    let meta = fs::metadata(path).ok()?;
    let name = entry_name(
        path,
        meta.len(),
        meta.created().ok(),
        ctime(&meta),
        meta.modified().ok(),
    );
    Some(cache.join(name))
}

/// Write an already encoded preview to `target` via a temp file and rename, so
/// yazi never sees a half-written entry. Existing entries are not pruned.
pub fn write_entry(target: &Path, bytes: &[u8]) -> io::Result<()> {
    static SEQ: AtomicU32 = AtomicU32::new(0);

    let dir = target
        .parent()
        .ok_or_else(|| io::Error::other("cache target has no parent"))?;
    let name = target
        .file_name()
        .ok_or_else(|| io::Error::other("cache target has no file name"))?
        .to_string_lossy();
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!(".{name}.{}.{n}.tmp", std::process::id()));

    if let Err(err) = fs::write(&tmp, bytes) {
        let _ = fs::remove_file(&tmp);
        return Err(err);
    }
    if let Err(err) = fs::rename(&tmp, target) {
        let _ = fs::remove_file(&tmp);
        return Err(err);
    }
    Ok(())
}

fn default_config_dir() -> Option<PathBuf> {
    env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .map(|p| p.join("yazi"))
        .or_else(|| {
            env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .map(|p| p.join(".config/yazi"))
        })
}

fn default_temp_cache_dir() -> PathBuf {
    env::temp_dir().join(format!("yazi-{}", rustix::process::getuid().as_raw()))
}

/// Read `[preview] cache_dir` from `yazi.toml`. An absent or empty value means
/// yazi's default temp directory.
fn cache_dir_from_config(config_dir: &Path) -> Option<PathBuf> {
    let text = fs::read_to_string(config_dir.join("yazi.toml")).ok()?;
    parse_cache_dir(&text)
}

fn parse_cache_dir(text: &str) -> Option<PathBuf> {
    let mut in_preview = false;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix('[') {
            in_preview = rest
                .split(']')
                .next()
                .is_some_and(|s| s.trim() == "preview");
            continue;
        }
        if !in_preview {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() != "cache_dir" {
            continue;
        }
        let value = unquote(strip_comment(value).trim());
        if value.is_empty() {
            return None;
        }
        let path = PathBuf::from(value);
        return path.is_absolute().then_some(path);
    }
    None
}

fn strip_comment(value: &str) -> &str {
    let mut quote = None;
    for (i, c) in value.char_indices() {
        match c {
            '"' | '\'' if quote.is_none() => quote = Some(c),
            c if quote == Some(c) => quote = None,
            '#' if quote.is_none() => return &value[..i],
            _ => {}
        }
    }
    value
}

fn unquote(value: &str) -> String {
    let bytes = value.as_bytes();
    if bytes.len() >= 2
        && ((bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\''))
    {
        let inner = &value[1..value.len() - 1];
        return if bytes[0] == b'"' {
            inner.replace("\\\"", "\"").replace("\\\\", "\\")
        } else {
            inner.to_owned()
        };
    }
    value.to_owned()
}

fn ctime(meta: &fs::Metadata) -> Option<SystemTime> {
    UNIX_EPOCH.checked_add(Duration::new(meta.ctime() as u64, meta.ctime_nsec() as u32))
}

/// Yazi's key: `Url::Regular` discriminant, `Path::hash`, then its `ChaSig`
/// (length, birth/change/modified times), then the preview `skip` offset.
fn entry_name(
    path: &Path,
    len: u64,
    btime: Option<SystemTime>,
    ctime: Option<SystemTime>,
    mtime: Option<SystemTime>,
) -> String {
    struct Hash128(XxHash3_128);

    impl Hasher for Hash128 {
        fn write(&mut self, bytes: &[u8]) {
            self.0.write(bytes);
        }

        fn finish(&self) -> u64 {
            self.0.finish_128() as u64
        }
    }

    let mut h = Hash128(XxHash3_128::new());
    0usize.hash(&mut h);
    path.hash(&mut h);
    len.hash(&mut h);
    btime.hash(&mut h);
    ctime.hash(&mut h);
    mtime.hash(&mut h);
    0usize.hash(&mut h);
    base32(&h.0.finish_128().to_be_bytes())
}

/// RFC 4648 base32 without padding (yazi's `BASE32_NOPAD`).
fn base32(data: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

    let mut out = String::with_capacity(data.len().div_ceil(5) * 8);
    let mut buffer: u32 = 0;
    let mut bits = 0;
    for &byte in data {
        buffer = (buffer << 8) | u32::from(byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((buffer >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((buffer << (5 - bits)) & 31) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base32_matches_rfc4648() {
        assert_eq!(base32(b"foobar"), "MZXW6YTBOI");
        assert_eq!(base32(&[0; 16]).len(), 26);
        assert_eq!(base32(&[0xff; 16]), "77777777777777777777777774");
    }

    #[test]
    fn entry_name_matches_yazi() {
        let path = Path::new("/tmp/rnn-yazi-test.png");
        let ctime = UNIX_EPOCH.checked_add(Duration::new(1_700_000_000, 123_456_789));
        let mtime = UNIX_EPOCH.checked_add(Duration::new(1_700_000_001, 987_654_321));
        assert_eq!(
            entry_name(path, 123_456, None, ctime, mtime),
            "TFRNHTDHF4L7BRMPZWSMVMK3WM"
        );
    }

    #[test]
    fn parses_preview_cache_dir() {
        assert_eq!(
            parse_cache_dir("[preview]\ncache_dir = \"/tmp/yazi-cache\" # note\n"),
            Some(PathBuf::from("/tmp/yazi-cache"))
        );
        assert_eq!(parse_cache_dir("[preview]\ncache_dir = \"\"\n"), None);
        assert_eq!(parse_cache_dir("[other]\ncache_dir = \"/nope\"\n"), None);
    }
}
