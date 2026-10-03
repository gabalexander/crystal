//! Where the daemon listens.

use anyhow::{Context, Result, ensure};
use std::ffi::OsString;
use std::fs::{self, DirBuilder};
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};

/// `$XDG_RUNTIME_DIR/crystal/default.sock`, or `/tmp/crystal-<uid>/default.sock`
/// where there's no runtime dir (macOS).
pub fn default_path() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(runtime) if !runtime.is_empty() => in_runtime_dir(Path::new(&runtime)),
        _ => in_tmp(uid()),
    }
}

/// Whether `socket` is the default socket, the user's own, however it's
/// spelled and whoever asks: the same socket has to get the same state
/// whoever starts its daemon, and the default socket's [`default_path`]
/// depends on the environment of whoever asks. So it's the default socket
/// in `/tmp`, or in systemd's runtime dir, `/run/user/<uid>`, whatever the
/// environment says, or in the `$XDG_RUNTIME_DIR` it names.
pub fn is_default(socket: &Path) -> bool {
    is_default_for(socket, std::env::var_os("XDG_RUNTIME_DIR"), uid())
}

/// [`is_default`] for the user `uid`, given the `$XDG_RUNTIME_DIR` of
/// whoever asks.
fn is_default_for(socket: &Path, runtime: Option<OsString>, uid: u32) -> bool {
    let mut defaults = vec![
        in_tmp(uid),
        in_runtime_dir(&PathBuf::from(format!("/run/user/{uid}"))),
    ];
    if let Some(runtime) = runtime.filter(|runtime| !runtime.is_empty()) {
        defaults.push(in_runtime_dir(Path::new(&runtime)));
    }
    let socket = resolved(socket);
    defaults.iter().any(|default| resolved(default) == socket)
}

/// The default socket in the runtime dir `runtime`.
fn in_runtime_dir(runtime: &Path) -> PathBuf {
    runtime.join("crystal").join("default.sock")
}

/// The default socket in `/tmp`, for the user `uid`.
fn in_tmp(uid: u32) -> PathBuf {
    PathBuf::from(format!("/tmp/crystal-{uid}/default.sock"))
}

/// `path` with the links in as much of it as exists followed, so that two
/// spellings of one file are the same: macOS's `/tmp` is `/private/tmp`.
fn resolved(path: &Path) -> PathBuf {
    for there in path.ancestors() {
        if let Ok(real) = fs::canonicalize(there) {
            return match path.strip_prefix(there) {
                Ok(rest) if !rest.as_os_str().is_empty() => real.join(rest),
                _ => real,
            };
        }
    }
    path.to_path_buf()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_socket_is_the_default_whatever_the_runtime_dir_of_whoever_asks() {
        // A daemon started on the user's socket by a process with a runtime
        // dir of its own once took it for another socket, and started with
        // none of the user's sessions.
        let socket = Path::new("/tmp/crystal-4242/default.sock");
        assert!(is_default_for(socket, Some("/tmp/a-test".into()), 4242));
        assert!(is_default_for(socket, None, 4242));
        let systemd = Path::new("/run/user/4242/crystal/default.sock");
        assert!(is_default_for(systemd, None, 4242));
        assert!(is_default_for(systemd, Some("/tmp/a-test".into()), 4242));
    }

    #[test]
    fn a_runtime_dir_of_its_own_has_the_default_socket_for_whoever_names_it() {
        let socket = Path::new("/home/ann/run/crystal/default.sock");
        assert!(is_default_for(socket, Some("/home/ann/run".into()), 4242));
        assert!(!is_default_for(socket, None, 4242));
    }

    #[test]
    fn any_other_socket_is_its_own() {
        let other = |socket: &str| !is_default_for(Path::new(socket), Some("".into()), 4242);
        assert!(other("/tmp/test/crystal.sock"));
        assert!(other("/tmp/crystal-4243/default.sock"));
        assert!(other("/tmp/crystal-4242/other.sock"));
        assert!(other("/tmp/run/crystal/default.sock"));
    }

    #[test]
    fn the_default_socket_is_the_default_however_it_s_spelled() {
        let dir = tempfile::tempdir().unwrap();
        let run = dir.path().join("run");
        fs::create_dir(&run).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&run, &link).unwrap();
        // Through a link, before the socket's directory exists.
        let socket = link.join("crystal/default.sock");
        assert!(is_default_for(&socket, Some(run.clone().into()), 4242));
        assert_eq!(
            resolved(&socket),
            fs::canonicalize(&run).unwrap().join("crystal/default.sock")
        );
        assert_eq!(resolved(&run), fs::canonicalize(&run).unwrap());
    }
}
