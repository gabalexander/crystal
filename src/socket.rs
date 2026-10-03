//! Where the daemon listens: a server's socket, named after it, in
//! crystal's socket directory, or a socket given by its path.
//!
//! A server is a daemon of its own, with its own sessions and state. The
//! default server is the one crystal uses unless told another; `--server`
//! names another, the way tmux's `-L` does.

use anyhow::{Context, Result, bail, ensure};
use std::ffi::OsString;
use std::fs::{self, DirBuilder};
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};

/// The server crystal uses unless told another.
pub const DEFAULT: &str = "default";

/// The longest name a server can have: its socket's path has to fit in the
/// hundred-odd bytes a unix socket's address holds.
const LONGEST_NAME: usize = 64;

/// The socket a command is for: `-S`'s, or else `--server`'s, or else
/// `$CRYSTAL_SOCKET`'s, which crystal gives each session, so that crystal
/// run in one reaches its own daemon, or else `$CRYSTAL_SERVER`'s, or else
/// the default server's.
pub fn chosen(socket: Option<PathBuf>, server: Option<String>) -> Result<PathBuf> {
    let env_socket = std::env::var_os("CRYSTAL_SOCKET").filter(|socket| !socket.is_empty());
    let env_server = std::env::var("CRYSTAL_SERVER")
        .ok()
        .filter(|name| !name.is_empty());
    choose(socket, server, env_socket.map(PathBuf::from), env_server)
}

/// [`chosen`], given the environment's `$CRYSTAL_SOCKET` and
/// `$CRYSTAL_SERVER`.
fn choose(
    socket: Option<PathBuf>,
    server: Option<String>,
    env_socket: Option<PathBuf>,
    env_server: Option<String>,
) -> Result<PathBuf> {
    match (socket, server, env_socket, env_server) {
        (Some(socket), ..) => Ok(socket),
        (None, Some(server), ..) => of_server(&server),
        (None, None, Some(socket), _) => Ok(socket),
        (None, None, None, Some(server)) => of_server(&server),
        (None, None, None, None) => Ok(default_path()),
    }
}

/// The socket of the server called `name`, in crystal's socket directory.
pub fn of_server(name: &str) -> Result<PathBuf> {
    check_name(name)?;
    Ok(in_dir(name))
}

/// `$XDG_RUNTIME_DIR/crystal/default.sock`, or `/tmp/crystal-<uid>/default.sock`
/// where there's no runtime dir (macOS).
pub fn default_path() -> PathBuf {
    in_dir(DEFAULT)
}

/// The socket of the server called `name`, a name already checked.
fn in_dir(name: &str) -> PathBuf {
    dir().join(format!("{name}.sock"))
}

/// crystal's socket directory: `$XDG_RUNTIME_DIR/crystal`, or
/// `/tmp/crystal-<uid>` where there's no runtime dir (macOS).
pub fn dir() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(runtime) if !runtime.is_empty() => dir_in_runtime(Path::new(&runtime)),
        _ => dir_in_tmp(uid()),
    }
}

/// Fails unless `name` can name a server. It names its socket and the
/// directory of its state, so it's kept to letters, digits, `-` and `_`.
pub fn check_name(name: &str) -> Result<()> {
    let starts_well = name.starts_with(|c: char| c.is_ascii_alphanumeric());
    let rest_fits = name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !starts_well || !rest_fits {
        bail!(
            "{name:?} can't name a server: a name is letters, digits, - and _, \
             starting with a letter or a digit"
        );
    }
    ensure!(
        name.len() <= LONGEST_NAME,
        "a server's name is {LONGEST_NAME} characters at most"
    );
    Ok(())
}

/// The name of the server whose socket `socket` is, if it's one in
/// crystal's socket directory, however it's spelled and whoever asks: the
/// same socket has to get the same state whoever starts its daemon, and
/// the socket directory depends on the environment of whoever asks. So
/// it's crystal's directory in `/tmp`, or in systemd's runtime dir,
/// `/run/user/<uid>`, whatever the environment says, or in the
/// `$XDG_RUNTIME_DIR` it names. `None` for a socket given by its path
/// anywhere else.
pub fn server_of(socket: &Path) -> Option<String> {
    server_of_for(socket, std::env::var_os("XDG_RUNTIME_DIR"), uid())
}

/// [`server_of`] for the user `uid`, given the `$XDG_RUNTIME_DIR` of
/// whoever asks.
fn server_of_for(socket: &Path, runtime: Option<OsString>, uid: u32) -> Option<String> {
    let name = socket.file_name()?.to_str()?.strip_suffix(".sock")?;
    check_name(name).ok()?;
    let mut dirs = vec![
        dir_in_tmp(uid),
        dir_in_runtime(&PathBuf::from(format!("/run/user/{uid}"))),
    ];
    if let Some(runtime) = runtime.filter(|runtime| !runtime.is_empty()) {
        dirs.push(dir_in_runtime(Path::new(&runtime)));
    }
    let there = resolved(socket.parent()?);
    let ours = dirs.iter().any(|dir| resolved(dir) == there);
    ours.then(|| name.to_string())
}

/// How the user runs crystal on the daemon at `socket`, for telling them
/// what to run: with `--server` for a server of their own, and plain
/// `crystal` for the default server, or for a socket given by its path,
/// which they know how they gave.
pub fn crystal_for(socket: &Path) -> String {
    match server_of(socket) {
        Some(server) if server != DEFAULT => format!("crystal --server {server}"),
        _ => "crystal".to_string(),
    }
}

/// crystal's socket directory in the runtime dir `runtime`.
fn dir_in_runtime(runtime: &Path) -> PathBuf {
    runtime.join("crystal")
}

/// crystal's socket directory in `/tmp`, for the user `uid`.
fn dir_in_tmp(uid: u32) -> PathBuf {
    PathBuf::from(format!("/tmp/crystal-{uid}"))
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

    fn is_default_for(socket: &Path, runtime: Option<OsString>, uid: u32) -> bool {
        server_of_for(socket, runtime, uid).as_deref() == Some(DEFAULT)
    }

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
    fn a_server_s_socket_is_named_after_it_in_the_socket_dir() {
        let server = |socket: &str, runtime: Option<&str>| {
            server_of_for(Path::new(socket), runtime.map(OsString::from), 4242)
        };
        assert_eq!(server("/tmp/crystal-4242/work.sock", None).unwrap(), "work");
        let systemd = "/run/user/4242/crystal/side-project.sock";
        assert_eq!(
            server(systemd, Some("/tmp/a-test")).unwrap(),
            "side-project"
        );
        let own = server("/home/ann/run/crystal/work.sock", Some("/home/ann/run"));
        assert_eq!(own.unwrap(), "work");
    }

    #[test]
    fn any_other_socket_is_its_own() {
        let own = |socket: &str| server_of_for(Path::new(socket), Some("".into()), 4242).is_none();
        assert!(own("/tmp/test/crystal.sock"));
        assert!(own("/tmp/crystal-4243/default.sock"));
        assert!(own("/tmp/run/crystal/default.sock"));
        assert!(own("/tmp/crystal-4242/default.log"));
        assert!(own("/tmp/crystal-4242/...sock"));
        assert!(own("/tmp/crystal-4242/.sock"));
        assert!(!is_default_for(
            Path::new("/tmp/crystal-4242/other.sock"),
            None,
            4242
        ));
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

    #[test]
    fn a_name_is_letters_digits_dashes_and_underscores() {
        for name in ["work", "side-project", "agents_2", "2026"] {
            assert!(check_name(name).is_ok(), "{name}");
        }
        let long = "a".repeat(LONGEST_NAME + 1);
        for name in [
            "", "..", "../work", "a/b", ".hidden", "-x", "a.b", "a b", &long,
        ] {
            assert!(check_name(name).is_err(), "{name}");
        }
    }

    #[test]
    fn a_flag_comes_before_the_environment_and_a_socket_before_a_server() {
        let work = of_server("work").unwrap();
        let path = || Some(PathBuf::from("/tmp/test/crystal.sock"));
        let named = |name: &str| Some(name.to_string());
        let chose = |socket, server, env_socket, env_server| {
            choose(socket, server, env_socket, env_server).unwrap()
        };
        assert_eq!(chose(path(), named("work"), None, None), path().unwrap());
        assert_eq!(chose(None, named("work"), path(), None), work);
        // A session's own daemon, before the server its shell may name.
        assert_eq!(chose(None, None, path(), named("work")), path().unwrap());
        assert_eq!(chose(None, None, None, named("work")), work);
        assert_eq!(chose(None, None, None, None), default_path());
        assert!(choose(None, named("../work"), None, None).is_err());
    }

    #[test]
    fn crystal_is_told_which_server_to_use_but_for_the_default() {
        assert_eq!(
            crystal_for(&of_server("work").unwrap()),
            "crystal --server work"
        );
        assert_eq!(crystal_for(&default_path()), "crystal");
        assert_eq!(crystal_for(Path::new("/tmp/test/crystal.sock")), "crystal");
    }
}
