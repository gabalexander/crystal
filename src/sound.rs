//! Sounds: a chime when a session comes to need the user, at the moments
//! [`crate::notify`] tells them: its agent asks them something, or is done
//! with a turn nobody watched. The system's own player plays it, so crystal
//! needs no audio library: `afplay` on macOS, and on Linux the first of
//! [`LINUX_PLAYERS`] that's installed and plays it.
//!
//! Two sounds come with crystal, from herdr (see `assets/sounds/NOTICE`):
//! one for an agent that's done, one for an agent that asks. The config's
//! `[sound]` can name files of the user's own instead, and switch sounds
//! off, for every agent or for some by their program.
//!
//! A sound never gets in the way: it plays on a thread of its own, a player
//! that hangs is stopped after [`PLAY_TIMEOUT`], and what goes wrong goes
//! to the daemon's log. `CRYSTAL_NO_SOUND`, set to anything, keeps crystal
//! quiet whatever the config says.

use crate::config::{Config, SoundSettings};
use crate::protocol::Activity;
use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

/// How long a player may take before it's stopped.
const PLAY_TIMEOUT: Duration = Duration::from_secs(15);

/// The environment variable that keeps crystal quiet.
const NO_SOUND: &str = "CRYSTAL_NO_SOUND";

static DONE: &[u8] = include_bytes!("../assets/sounds/done.mp3");
static REQUEST: &[u8] = include_bytes!("../assets/sounds/request.mp3");

/// Which sound to play.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sound {
    /// An agent finished a turn nobody watched.
    Done,
    /// An agent is asking the user something.
    Request,
}

impl Sound {
    /// The sound for a session whose agent has come to do `activity`.
    pub fn of(activity: Activity) -> Sound {
        match activity {
            Activity::Waiting => Sound::Request,
            _ => Sound::Done,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Sound::Done => "done",
            Sound::Request => "request",
        }
    }

    /// The sound as it comes with crystal: an mp3.
    fn bundled(self) -> &'static [u8] {
        match self {
            Sound::Done => DONE,
            Sound::Request => REQUEST,
        }
    }

    /// The user's own file for it, where the config names one.
    fn own(self, settings: &SoundSettings) -> Option<&Path> {
        match self {
            Sound::Done => settings.done.as_deref(),
            Sound::Request => settings.request.as_deref(),
        }
    }
}

/// Whether a sound plays for a session whose agent's program is `agent`:
/// the notifications plugin is on, and sounds are, for that agent or for
/// all of them; and `CRYSTAL_NO_SOUND` isn't set.
pub fn wanted(config: &Config, agent: Option<&str>) -> bool {
    let muted = std::env::var_os(NO_SOUND).is_some();
    !muted && crate::plugins::enabled(config, "notifications") && on_for(&config.sound, agent)
}

/// Whether `settings` have sounds on for `agent`: its own switch, or else
/// the one for every agent.
fn on_for(settings: &SoundSettings, agent: Option<&str>) -> bool {
    agent
        .and_then(|agent| settings.agents.get(agent))
        .copied()
        .unwrap_or(settings.enabled)
}

/// Plays `sound` on a thread of its own: the user's own file for it when
/// the settings name one that plays, or else crystal's.
pub fn play(sound: Sound, settings: &SoundSettings) {
    let own = sound.own(settings).map(|path| resolve(path, &config_dir()));
    thread::spawn(move || {
        if let Err(err) = play_now(sound, own.as_deref()) {
            eprintln!(
                "crystal daemon: couldn't play the {} sound: {err:#}",
                sound.name()
            );
        }
    });
}

fn play_now(sound: Sound, own: Option<&Path>) -> Result<()> {
    if let Some(path) = own {
        match play_file(path) {
            Ok(()) => return Ok(()),
            Err(err) => eprintln!(
                "crystal daemon: couldn't play {}, crystal's own sound instead: {err:#}",
                path.display()
            ),
        }
    }
    play_file(&bundled_file(sound, &std::env::temp_dir())?)
}

/// The directory the config file is in, which a relative sound file's path
/// starts from.
fn config_dir() -> PathBuf {
    let path = crate::config::path();
    path.parent().map(Path::to_path_buf).unwrap_or_default()
}

/// Where `path`, as the config gives it, is: from the home directory for
/// `~/`, as given when absolute, or else from `dir`.
fn resolve(path: &Path, dir: &Path) -> PathBuf {
    if let Ok(rest) = path.strip_prefix("~") {
        let home = std::env::var_os("HOME").unwrap_or_default();
        return PathBuf::from(home).join(rest);
    }
    dir.join(path)
}

/// Crystal's own `sound`, written to a file in `dir` for a player to read:
/// once, named after what's in it, so that each sound is written only the
/// first time it plays, and a newer crystal's sound never plays an older
/// one's file. It's written beside its place and moved in, so a player
/// never reads half of it.
fn bundled_file(sound: Sound, dir: &Path) -> Result<PathBuf> {
    let bytes = sound.bundled();
    let hash = Sha256::digest(bytes);
    let short: String = hash[..6].iter().map(|byte| format!("{byte:02x}")).collect();
    let path = dir.join(format!("crystal-{}-{short}.mp3", sound.name()));
    if path.is_file() {
        return Ok(path);
    }
    // Each writer its own, so two sounds played at once never write one
    // file together.
    static WRITES: AtomicU64 = AtomicU64::new(0);
    let writing = dir.join(format!(
        "crystal-{}-{short}.{}-{}.part",
        sound.name(),
        std::process::id(),
        WRITES.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&writing, bytes)
        .with_context(|| format!("couldn't write {}", writing.display()))?;
    std::fs::rename(&writing, &path)
        .with_context(|| format!("couldn't write {}", path.display()))?;
    Ok(path)
}

/// A program that plays a sound file given as its last argument.
#[derive(Debug, Clone, Copy)]
struct Player {
    program: &'static str,
    args: &'static [&'static str],
}

/// The players tried on Linux, in order. Each decodes an mp3: `aplay`
/// doesn't, and would play its bytes as noise.
const LINUX_PLAYERS: &[Player] = &[
    Player {
        program: "paplay",
        args: &[],
    },
    Player {
        program: "pw-play",
        args: &[],
    },
    Player {
        program: "ffplay",
        args: &["-nodisp", "-autoexit", "-loglevel", "quiet"],
    },
    Player {
        program: "mpg123",
        args: &["-q"],
    },
    Player {
        program: "mpv",
        args: &["--no-video", "--really-quiet"],
    },
];

const MACOS_PLAYERS: &[Player] = &[Player {
    program: "afplay",
    args: &[],
}];

fn players() -> &'static [Player] {
    if cfg!(target_os = "macos") {
        MACOS_PLAYERS
    } else {
        LINUX_PLAYERS
    }
}

/// Plays the file at `path` with the first player that's installed and
/// plays it. With none installed, there's nothing to be done, and nothing
/// to say.
fn play_file(path: &Path) -> Result<()> {
    let mut failures = Vec::new();
    for player in players() {
        match player.play(path, PLAY_TIMEOUT) {
            Ok(()) => return Ok(()),
            Err(Failure::Missing) => {}
            Err(Failure::Failed(why)) => failures.push(format!("{}: {why}", player.program)),
        }
    }
    if failures.is_empty() {
        return Ok(());
    }
    bail!("{}", failures.join("; "))
}

/// Why a player didn't play a sound.
#[derive(Debug)]
enum Failure {
    /// It isn't installed.
    Missing,
    Failed(String),
}

impl Player {
    /// Plays `path`, and stops the player if it takes longer than
    /// `timeout`.
    fn play(self, path: &Path, timeout: Duration) -> Result<(), Failure> {
        let spawned = Command::new(self.program)
            .args(self.args)
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        let child = match spawned {
            Ok(child) => child,
            Err(err) if err.kind() == ErrorKind::NotFound => return Err(Failure::Missing),
            Err(err) => return Err(Failure::Failed(err.to_string())),
        };
        wait(child, timeout).map_err(|err| Failure::Failed(format!("{err:#}")))
    }
}

/// Waits for `child` to end, and stops it if it takes longer than
/// `timeout`.
fn wait(mut child: Child, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            if status.success() {
                return Ok(());
            }
            bail!("it {status}");
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("it took longer than {}s", timeout.as_secs_f32());
        }
        thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn an_agent_asking_requests_and_one_finishing_is_done() {
        assert_eq!(Sound::of(Activity::Waiting), Sound::Request);
        assert_eq!(Sound::of(Activity::Done), Sound::Done);
    }

    #[test]
    fn an_agent_s_own_switch_outweighs_the_one_for_all() {
        let settings = SoundSettings {
            agents: BTreeMap::from([("codex".to_string(), false), ("pi".to_string(), true)]),
            ..SoundSettings::default()
        };
        assert!(on_for(&settings, Some("claude")));
        assert!(on_for(&settings, None));
        assert!(!on_for(&settings, Some("codex")));
        let off = SoundSettings {
            enabled: false,
            ..settings
        };
        assert!(!on_for(&off, Some("claude")));
        assert!(on_for(&off, Some("pi")));
    }

    #[test]
    fn sounds_are_off_with_the_notifications_plugin() {
        let mut config = Config::default();
        config.plugins.insert("notifications".into(), false);
        assert!(!wanted(&config, Some("claude")));
    }

    #[test]
    fn a_sound_file_is_found_from_the_config_s_directory_or_home() {
        let dir = Path::new("/home/ann/.config/crystal");
        assert_eq!(
            resolve(Path::new("sounds/ding.mp3"), dir),
            dir.join("sounds/ding.mp3")
        );
        assert_eq!(
            resolve(Path::new("/usr/share/ding.wav"), dir),
            PathBuf::from("/usr/share/ding.wav")
        );
        let home = std::env::var_os("HOME").unwrap_or_default();
        assert_eq!(
            resolve(Path::new("~/ding.mp3"), dir),
            PathBuf::from(home).join("ding.mp3")
        );
    }

    #[test]
    fn crystal_s_own_sound_is_written_once_whole() {
        let dir = std::env::temp_dir().join(format!("crystal-sound-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = bundled_file(Sound::Request, &dir).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), REQUEST);
        let written = std::fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(bundled_file(Sound::Request, &dir).unwrap(), path);
        assert_eq!(
            std::fs::metadata(&path).unwrap().modified().unwrap(),
            written
        );
        let done = bundled_file(Sound::Done, &dir).unwrap();
        assert_ne!(done, path);
        // Nothing half-written is left behind.
        let left: Vec<_> = std::fs::read_dir(&dir).unwrap().collect();
        assert_eq!(left.len(), 2);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_player_that_isn_t_installed_is_missing() {
        let player = Player {
            program: "crystal-no-such-player",
            args: &[],
        };
        assert!(matches!(
            player.play(Path::new("x.mp3"), PLAY_TIMEOUT),
            Err(Failure::Missing)
        ));
    }

    #[test]
    fn a_player_that_hangs_is_stopped() {
        let player = Player {
            program: "sh",
            args: &["-c", "sleep 5", "player"],
        };
        let started = Instant::now();
        let played = player.play(Path::new("x.mp3"), Duration::from_millis(100));
        assert!(matches!(played, Err(Failure::Failed(why)) if why.contains("longer")));
        assert!(started.elapsed() < Duration::from_secs(4));
    }

    #[test]
    fn a_player_that_fails_says_how() {
        let player = Player {
            program: "sh",
            args: &["-c", "exit 3", "player"],
        };
        let played = player.play(Path::new("x.mp3"), PLAY_TIMEOUT);
        assert!(matches!(played, Err(Failure::Failed(why)) if why.contains('3')));
    }
}
