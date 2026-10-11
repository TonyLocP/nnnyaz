//! Minimal POSIX shared-memory writer for the kitty graphics `t=s` medium.
//!
//! The terminal reads the raw pixels straight out of `/dev/shm` and unlinks the
//! object, so nothing multi-megabyte ever crosses the pty. This is what makes
//! image previews instant under tmux, where a chunked base64 transmit is slow.

use std::{
    io,
    sync::atomic::{AtomicU32, Ordering},
};

/// Create a named POSIX shared memory object holding `data` and return the
/// name (`/rnn-...`) plus its byte length. The name is handed to the terminal,
/// which `shm_open`s then `shm_unlink`s it. Returns an error when shared memory
/// is unavailable (non-Linux, unsupported filesystem, no permission).
pub fn create(data: &[u8]) -> io::Result<String> {
    if data.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty shm payload"));
    }
    #[cfg(target_os = "linux")]
    {
        linux::create(data)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = data;
        Err(io::Error::new(io::ErrorKind::Unsupported, "shared memory is Linux-only"))
    }
}

/// Remove a shared memory object by name. Errors (including "not found" when
/// the terminal already unlinked it) are ignored by callers.
pub fn remove(name: &str) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        rustix::shm::unlink(name).map_err(Into::into)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = name;
        Ok(())
    }
}

fn unique_name() -> String {
    static N: AtomicU32 = AtomicU32::new(0);
    let pid = std::process::id();
    let n = N.fetch_add(1, Ordering::Relaxed);
    format!("/rnn-{pid}-{n}")
}

#[cfg(target_os = "linux")]
mod linux {
    use super::unique_name;
    use rustix::{
        fs::{ftruncate, Mode},
        io as rio,
        shm::{self, OFlags},
    };
    use std::io;

    pub fn create(data: &[u8]) -> io::Result<String> {
        let name = unique_name();
        let fd = shm::open(
            &*name,
            OFlags::CREATE | OFlags::EXCL | OFlags::RDWR,
            Mode::RUSR | Mode::WUSR,
        )?;

        if let Err(e) = ftruncate(&fd, data.len() as u64) {
            let _ = shm::unlink(&*name);
            return Err(e.into());
        }

        let mut written = 0usize;
        while written < data.len() {
            match rio::write(&fd, &data[written..]) {
                Ok(0) => {
                    let _ = shm::unlink(&*name);
                    return Err(io::Error::new(io::ErrorKind::WriteZero, "short shm write"));
                }
                Ok(n) => written += n,
                Err(e) => {
                    let _ = shm::unlink(&*name);
                    return Err(e.into());
                }
            }
        }

        // Close our descriptor; the terminal owns the name now. If it never
        // reads the object the name leaks, but `create` only runs when the
        // startup probe confirmed the terminal supports shared memory.
        drop(fd);
        Ok(name)
    }
}
