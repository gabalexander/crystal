//! Where the daemon listens.

use anyhow::{Context, Result, ensure};
use std::fs::DirBuilder;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};

/// `$XDG_RUNTIME_DIR/crystal/default.sock`, or `/tmp/crystal-<uid>/default.sock`
/// where there's no runtime dir (macOS).
pub fn default_path() -> PathBuf {
    let dir = match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(runtime) if !runtime.is_empty() => PathBuf::from(runtime).join("crystal"),
        _ => PathBuf::from(format!("/tmp/crystal-{}", uid())),
    };
    dir.join("default.sock")
}

/// The log the daemon writes to, beside its socket.
pub fn log_path(socket: &Path) -> PathBuf {
    socket.with_extension("log")
}

/// Creates the socket's directory, private to us, and refuses one that
/// belongs to someone else: whoever can reach the socket can run commands
/// as us.
pub fn prepare_dir(socket: &Path) -> Result<()> {
    let dir = socket
        .parent()
        .context("the socket path has no directory")?;
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .with_context(|| format!("couldn't create {}", dir.display()))?;
    let owner = dir.metadata()?.uid();
    ensure!(
        owner == uid(),
        "{} belongs to another user (uid {owner})",
        dir.display()
    );
    Ok(())
}

fn uid() -> u32 {
    // SAFETY: getuid has no preconditions and can't fail.
    unsafe { libc::getuid() }
}
