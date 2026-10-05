//! Files dropped on a text box whose text goes to an agent, the new-session
//! panel's task and the reply box, kept where the agent can find them;
//! adapted from docket's.
//!
//! A terminal hands a drop over as a paste of the file's path, escaped the
//! way a shell reads it. A macOS screenshot dragged from its floating
//! thumbnail comes as
//!
//! ```text
//! /var/folders/…/T/TemporaryItems/NSIRD_screencaptureui_LySI4r/Screenshot\ 2026-09-21\ at\ 11.13.58 PM.png
//! ```
//!
//! and that path, sent on as it came, lost the screenshot twice over. The
//! file behind the thumbnail is macOS's to delete, and it does, soon after
//! the drop, long before the task typed around it starts an agent. And the
//! name holds a U+202F NARROW NO-BREAK SPACE before `PM`, which an agent
//! types back as a plain space, so its `Read` misses a file that's there.
//!
//! So a paste into one of those boxes that is nothing but the paths of
//! files is staged as it lands: a file in a folder that goes away (a
//! `TemporaryItems` folder anywhere, or on a Mac the temporary directory
//! macOS clears), and an image whose path an agent can't type back as it
//! is, are copied into the server's `attachments` directory under a plain
//! name, and the box gets the copy's path in place of the one dropped. Any
//! other file, a source file in the worktree or an image with a tidy name,
//! is left as pasted: the agent should work on the file itself.
//!
//! A copy is kept for [`KEEP`] from when it was last dropped: older ones
//! are deleted as another is copied, and as the TUI starts.

use std::fs;
use std::io;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime};

/// How long a copy outlives its drop. A task is started within minutes; a
/// week covers a session picked up again days later that looks at it again.
const KEEP: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// A paste longer than this is no drop: no terminal sends a wall of paths,
/// and an everyday paste isn't looked through.
const MAX_DROP_LEN: usize = 16 * 1024;

/// The largest copy compared byte for byte with a new drop of its name; a
/// bigger one just gets a numbered copy beside it.
const MAX_COMPARE: u64 = 64 * 1024 * 1024;

/// The extensions of files an agent is shown, as images.
const IMAGE_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "heic", "heif", "tif", "tiff", "bmp",
];

/// Where dropped files are copied, and the folder beside `TemporaryItems`
/// they're copied out of.
#[derive(Debug, Clone)]
pub struct Drops {
    /// The directory the copies go in.
    dir: PathBuf,
    /// A folder the system empties behind the user's back: on a Mac, the
    /// temporary directory under `/var/folders`, which apps drag files out
    /// of and nobody puts a file in by hand. Not `/tmp` elsewhere, where
    /// people do.
    temporary: Option<PathBuf>,
}

/// A drop, staged.
#[derive(Debug, PartialEq, Eq)]
pub struct Staged {
    /// The paste as it goes into the box.
    pub text: String,
    /// The files that had to be copied and couldn't be, by name, each with
    /// why. Their paths stay in `text` as they were pasted.
    pub failed: Vec<(String, String)>,
}

impl Drops {
    /// Copies kept in `dir`, out of `TemporaryItems` folders and, on a Mac,
    /// the temporary directory.
    pub fn new(dir: PathBuf) -> Drops {
        let temporary = cfg!(target_os = "macos").then(|| {
            let temp = std::env::temp_dir();
            fs::canonicalize(&temp).unwrap_or(temp)
        });
        Drops { dir, temporary }
    }

    /// `text` staged for a box bound for an agent, or `None` when the paste
    /// is no drop (anything but paths of files that are there, apart by
    /// whitespace), or one with nothing to copy: it goes in as it came.
    pub fn stage(&self, text: &str) -> Option<Staged> {
        let body = text.trim();
        if body.is_empty()
            || body.len() > MAX_DROP_LEN
            || !body.starts_with(['/', '~', '\'', '"', 'f'])
        {
            return None;
        }
        let words = split_words(body)?;
        let files: Vec<PathBuf> = words
            .iter()
            .map(|(_, word)| dropped_file(word))
            .collect::<Option<_>>()?;
        if !files.iter().any(|file| self.needs_copy(file)) {
            return None;
        }
        self.prune();
        let mut failed = Vec::new();
        let mut staged = String::new();
        for ((raw, _), file) in words.iter().zip(&files) {
            if !staged.is_empty() {
                staged.push(' ');
            }
            if !self.needs_copy(file) {
                staged.push_str(raw);
                continue;
            }
            match self.copy_in(file) {
                Ok(copy) => staged.push_str(&crate::shell::quote(&copy.to_string_lossy())),
                Err(err) => {
                    staged.push_str(raw);
                    failed.push((file_name(file), err.to_string()));
                }
            }
        }
        // Whatever whitespace framed the paste still frames it, like the
        // space some terminals put after a dropped path.
        let lead = &text[..text.len() - text.trim_start().len()];
        let trail = &text[text.trim_end().len()..];
        Some(Staged {
            text: format!("{lead}{staged}{trail}"),
            failed,
        })
    }

    /// Deletes the copies older than [`KEEP`]. As best it can: a copy that
    /// can't be looked at or removed is left for the next time.
    pub fn prune(&self) {
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return;
        };
        let now = SystemTime::now();
        for entry in entries.flatten() {
            let old = entry
                .metadata()
                .ok()
                .filter(|meta| meta.is_file())
                .and_then(|meta| meta.modified().ok())
                .and_then(|modified| now.duration_since(modified).ok())
                .is_some_and(|age| age > KEEP);
            if old {
                let _ = fs::remove_file(entry.path());
            }
        }
    }

    /// Whether `file` has to be copied before an agent can use it: it's in
    /// a folder that's emptied behind a drag, a screenshot's floating
    /// thumbnail's, or it's an image whose path an agent won't type back
    /// right.
    fn needs_copy(&self, file: &Path) -> bool {
        let fleeting = file
            .components()
            .any(|part| part == Component::Normal("TemporaryItems".as_ref()))
            || self.temporary.as_ref().is_some_and(|temporary| {
                fs::canonicalize(file).is_ok_and(|file| file.starts_with(temporary))
            });
        fleeting || (is_image(file) && !plain(&file.to_string_lossy()))
    }

    /// Copies `file` into the directory under its name made plain, and
    /// gives the copy. A copy there already with the same bytes is used
    /// again, so one screenshot dropped twice leaves one file; another file
    /// of that name gets a numbered one beside it.
    fn copy_in(&self, file: &Path) -> io::Result<PathBuf> {
        // Only the user reads them: a screenshot can show anything.
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.dir)?;
        let (stem, extension) = plain_name(&file_name(file));
        let len = fs::metadata(file)?.len();
        for n in 1.. {
            let copy = match n {
                1 => self.dir.join(format!("{stem}{extension}")),
                n => self.dir.join(format!("{stem}-{n}{extension}")),
            };
            match fs::metadata(&copy) {
                Ok(there) if there.len() == len && same_bytes(file, &copy) => {
                    return touched(copy);
                }
                Ok(_) => continue,
                Err(err) if err.kind() == io::ErrorKind::NotFound => {
                    // A clone on APFS, so a big drop takes no time; it keeps
                    // the dropped file's time, which `touched` makes now.
                    fs::copy(file, &copy)?;
                    return touched(copy);
                }
                Err(err) => return Err(err),
            }
        }
        unreachable!("counting up never runs out")
    }
}

/// The paste split into words the way a shell reads them, a `\` taking
/// the character after it as it is and `'…'` and `"…"` quoting, at ASCII
/// whitespace alone: the U+202F in a screenshot's name is part of the
/// name. Each word as pasted and as read; `None` for a quote left open or
/// a `\` at the end.
fn split_words(text: &str) -> Option<Vec<(&str, String)>> {
    let mut words = Vec::new();
    let mut start = None;
    let mut word = String::new();
    let mut quote: Option<char> = None;
    let mut chars = text.char_indices();
    while let Some((at, c)) = chars.next() {
        match (quote, c) {
            (Some(open), c) if c == open => quote = None,
            (Some('"'), '\\') | (None, '\\') => word.push(chars.next()?.1),
            (Some(_), c) => word.push(c),
            (None, c) if c.is_ascii_whitespace() => {
                if let Some(from) = start.take() {
                    words.push((&text[from..at], std::mem::take(&mut word)));
                }
                continue;
            }
            (None, '\'' | '"') => quote = Some(c),
            (None, c) => word.push(c),
        }
        start.get_or_insert(at);
    }
    if quote.is_some() {
        return None;
    }
    if let Some(from) = start {
        words.push((&text[from..], word));
    }
    Some(words)
}

/// The file a dropped word names, an absolute path, `~/…` or a `file://`
/// URL, when it's a file that's there.
fn dropped_file(word: &str) -> Option<PathBuf> {
    let path = if let Some(url) = word.strip_prefix("file://") {
        PathBuf::from(percent_decoded(
            url.strip_prefix("localhost").unwrap_or(url),
        )?)
    } else if word.starts_with("~/") {
        crate::shell::expand_home(Path::new(word))
    } else {
        PathBuf::from(word)
    };
    (path.is_absolute() && path.is_file()).then_some(path)
}

/// A `file://` URL's path with its `%XX` escapes read.
fn percent_decoded(path: &str) -> Option<String> {
    let mut bytes = Vec::with_capacity(path.len());
    let mut rest = path.as_bytes();
    while let Some((&byte, tail)) = rest.split_first() {
        if byte == b'%' {
            let hex = std::str::from_utf8(tail.get(..2)?).ok()?;
            bytes.push(u8::from_str_radix(hex, 16).ok()?);
            rest = &tail[2..];
        } else {
            bytes.push(byte);
            rest = tail;
        }
    }
    String::from_utf8(bytes).ok()
}

fn is_image(file: &Path) -> bool {
    file.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            IMAGE_EXTENSIONS
                .iter()
                .any(|image| extension.eq_ignore_ascii_case(image))
        })
}

/// Whether `path` can go into a prompt as it is: nothing a shell would
/// want escaped, nothing outside ASCII.
fn plain(path: &str) -> bool {
    path.chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._-+@,:=~%".contains(c))
}

/// Whether two files of one length hold the same bytes. Past
/// [`MAX_COMPARE`] they're taken to differ rather than read into memory.
fn same_bytes(a: &Path, b: &Path) -> bool {
    let small = fs::metadata(a).is_ok_and(|meta| meta.len() <= MAX_COMPARE);
    small && matches!((fs::read(a), fs::read(b)), (Ok(a), Ok(b)) if a == b)
}

/// `copy` with its time made now, so a copy dropped again today isn't
/// deleted a week after it was first made.
fn touched(copy: PathBuf) -> io::Result<PathBuf> {
    fs::File::options()
        .write(true)
        .open(&copy)?
        .set_modified(SystemTime::now())?;
    Ok(copy)
}

fn file_name(file: &Path) -> String {
    file.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// A file's name made plain, as its stem and its `.extension`: each run of
/// anything but ASCII letters, digits, `.`, `_` and `-` made one `-`, so
/// `Screenshot 2026-09-21 at 11.13.58 PM.png` (U+202F before `PM`) is
/// `Screenshot-2026-09-21-at-11.13.58-PM.png`.
fn plain_name(name: &str) -> (String, String) {
    let (stem, extension) = match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => (stem, Some(extension)),
        _ => (name, None),
    };
    let stem = match plain_run(stem) {
        stem if stem.is_empty() => "dropped".to_string(),
        stem => stem,
    };
    let extension = extension
        .map(plain_run)
        .filter(|extension| !extension.is_empty())
        .map(|extension| format!(".{extension}"))
        .unwrap_or_default();
    (stem, extension)
}

fn plain_run(text: &str) -> String {
    let mut plain = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
            plain.push(c);
        } else if !plain.ends_with('-') {
            plain.push('-');
        }
    }
    plain.trim_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Copies kept in `root`'s `attachments`, out of `root`'s `T`, the way
    /// a Mac's temporary directory would be.
    fn drops(root: &Path) -> Drops {
        Drops {
            dir: root.join("attachments"),
            temporary: Some(fs::canonicalize(root).unwrap().join("T")),
        }
    }

    /// A path escaped for a shell, the way Ghostty pastes a drop.
    fn escaped(path: &Path) -> String {
        let mut escaped = String::new();
        for c in path.to_string_lossy().chars() {
            if " ()'\"\\".contains(c) {
                escaped.push('\\');
            }
            escaped.push(c);
        }
        escaped
    }

    /// A macOS screenshot behind its floating thumbnail, in `root`.
    fn thumbnail(root: &Path) -> PathBuf {
        let dir = root.join("T/TemporaryItems/NSIRD_screencaptureui_LySI4r");
        fs::create_dir_all(&dir).unwrap();
        let shot = dir.join("Screenshot 2026-09-21 at 11.13.58\u{202f}PM.png");
        fs::write(&shot, b"\x89PNG pixels").unwrap();
        shot
    }

    fn copied(root: &Path, name: &str) -> PathBuf {
        root.join("attachments").join(name)
    }

    #[test]
    fn a_dropped_screenshot_is_copied_under_a_plain_name_before_macos_deletes_it() {
        let root = tempfile::tempdir().unwrap();
        let shot = thumbnail(root.path());

        let staged = drops(root.path()).stage(&escaped(&shot)).expect("a drop");

        let copy = copied(root.path(), "Screenshot-2026-09-21-at-11.13.58-PM.png");
        assert_eq!(staged.text, crate::shell::quote(&copy.to_string_lossy()));
        assert!(staged.failed.is_empty());
        // macOS deletes the thumbnail's file; the copy is what the agent reads.
        fs::remove_file(&shot).unwrap();
        assert_eq!(fs::read(&copy).unwrap(), b"\x89PNG pixels");
        let mode = fs::metadata(root.path().join("attachments"))
            .unwrap()
            .permissions();
        assert_eq!(
            std::os::unix::fs::PermissionsExt::mode(&mode) & 0o777,
            0o700
        );
    }

    #[test]
    fn any_file_in_the_temporary_directory_is_copied_and_whitespace_around_it_kept() {
        let root = tempfile::tempdir().unwrap();
        let dragged = root.path().join("T/com.tinyspeck.slackmacgap/report.pdf");
        fs::create_dir_all(dragged.parent().unwrap()).unwrap();
        fs::write(&dragged, "pdf").unwrap();

        let staged = drops(root.path())
            .stage(&format!("{} ", dragged.display()))
            .unwrap();

        let copy = copied(root.path(), "report.pdf");
        assert_eq!(
            staged.text,
            format!("{} ", crate::shell::quote(&copy.to_string_lossy()))
        );
        assert!(copy.is_file());
    }

    #[test]
    fn an_image_with_an_awkward_name_outside_temp_is_copied_too() {
        let root = tempfile::tempdir().unwrap();
        let desktop = root.path().join("Desktop");
        fs::create_dir_all(&desktop).unwrap();
        let shot = desktop.join("Screenshot 2026-09-21 at 9.19.29\u{202f}PM.png");
        fs::write(&shot, b"png").unwrap();

        let staged = drops(root.path()).stage(&escaped(&shot)).unwrap();

        assert!(
            staged
                .text
                .contains("Screenshot-2026-09-21-at-9.19.29-PM.png"),
            "{:?}",
            staged.text
        );
        // The file stays where it was: a copy, never a move.
        assert!(shot.is_file());
    }

    #[test]
    fn a_file_the_agent_can_use_where_it_is_is_pasted_as_it_came() {
        let root = tempfile::tempdir().unwrap();
        let drops = drops(root.path());
        let source = root.path().join("src main.rs");
        fs::write(&source, "fn main() {}").unwrap();
        let tidy = root.path().join("logo.png");
        fs::write(&tidy, b"png").unwrap();

        // A source file, a space in its name or not: the agent works on it,
        // not a copy.
        assert_eq!(drops.stage(&escaped(&source)), None);
        // An image whose path is plain already.
        if plain(&tidy.to_string_lossy()) {
            assert_eq!(drops.stage(&escaped(&tidy)), None);
        }
        // Nowhere is temporary off a Mac but a `TemporaryItems` folder.
        let anywhere = Drops {
            temporary: None,
            ..drops.clone()
        };
        let dragged = root.path().join("T/notes.txt");
        fs::create_dir_all(dragged.parent().unwrap()).unwrap();
        fs::write(&dragged, "notes").unwrap();
        assert_eq!(anywhere.stage(&escaped(&dragged)), None);
        assert!(!root.path().join("attachments").exists(), "nothing copied");
    }

    #[test]
    fn several_dropped_files_are_staged_one_by_one() {
        let root = tempfile::tempdir().unwrap();
        let shot = thumbnail(root.path());
        let source = root.path().join("notes.txt");
        fs::write(&source, "notes").unwrap();

        let paste = format!("{} {}", escaped(&source), escaped(&shot));
        let staged = drops(root.path()).stage(&paste).unwrap();

        let copy = copied(root.path(), "Screenshot-2026-09-21-at-11.13.58-PM.png");
        assert_eq!(
            staged.text,
            format!(
                "{} {}",
                escaped(&source),
                crate::shell::quote(&copy.to_string_lossy())
            )
        );
    }

    #[test]
    fn text_that_isnt_only_paths_of_files_that_are_there_is_no_drop() {
        let root = tempfile::tempdir().unwrap();
        let drops = drops(root.path());
        let shot = thumbnail(root.path());

        for paste in [
            "fix the login redirect".to_string(),
            format!("{} what is this", escaped(&shot)),
            root.path().join("T/gone.png").display().to_string(),
            root.path().join("T").display().to_string(),
            "/".to_string(),
            "'/unterminated".to_string(),
            String::new(),
        ] {
            assert_eq!(drops.stage(&paste), None, "{paste:?}");
        }
    }

    #[test]
    fn quoted_and_file_url_drops_are_read_too() {
        let root = tempfile::tempdir().unwrap();
        let drops = drops(root.path());
        let shot = thumbnail(root.path());
        let path = shot.to_string_lossy().into_owned();
        let url = format!(
            "file://{}",
            path.replace(' ', "%20").replace('\u{202f}', "%E2%80%AF")
        );

        for paste in [format!("'{path}'"), format!("\"{path}\""), url] {
            let staged = drops.stage(&paste).expect(&paste);
            assert!(
                staged.text.contains("-PM.png"),
                "{paste:?} → {:?}",
                staged.text
            );
        }
        // Three drops of one screenshot, one copy.
        assert_eq!(
            fs::read_dir(root.path().join("attachments"))
                .unwrap()
                .count(),
            1
        );
    }

    #[test]
    fn another_file_of_a_name_taken_gets_a_numbered_copy() {
        let root = tempfile::tempdir().unwrap();
        let drops = drops(root.path());
        let shot = thumbnail(root.path());
        drops.stage(&escaped(&shot)).unwrap();
        fs::write(&shot, b"other pixels").unwrap();

        let staged = drops.stage(&escaped(&shot)).unwrap();

        assert!(
            staged
                .text
                .contains("Screenshot-2026-09-21-at-11.13.58-PM-2.png"),
            "{:?}",
            staged.text
        );
    }

    #[test]
    fn a_copy_that_cant_be_made_leaves_the_path_as_dropped_and_says_why() {
        let root = tempfile::tempdir().unwrap();
        let shot = thumbnail(root.path());
        // A file where the directory of copies would be.
        let blocked = Drops {
            dir: root.path().join("blocked"),
            ..drops(root.path())
        };
        fs::write(root.path().join("blocked"), "").unwrap();

        let staged = blocked.stage(&escaped(&shot)).unwrap();

        assert_eq!(staged.text, escaped(&shot));
        assert_eq!(staged.failed.len(), 1);
        assert_eq!(
            staged.failed[0].0,
            "Screenshot 2026-09-21 at 11.13.58\u{202f}PM.png"
        );
    }

    #[test]
    fn copies_older_than_a_week_are_deleted_by_the_next_drop_and_at_start() {
        let root = tempfile::tempdir().unwrap();
        let drops = drops(root.path());
        let attachments = root.path().join("attachments");
        fs::create_dir_all(&attachments).unwrap();
        let aged = |name: &str| {
            let old = attachments.join(name);
            fs::write(&old, b"old").unwrap();
            fs::File::options()
                .write(true)
                .open(&old)
                .unwrap()
                .set_modified(SystemTime::now() - KEEP - Duration::from_secs(60))
                .unwrap();
            old
        };
        let old = aged("old.png");
        let fresh = attachments.join("fresh.png");
        fs::write(&fresh, b"fresh").unwrap();

        drops.stage(&escaped(&thumbnail(root.path()))).unwrap();
        assert!(!old.exists());
        assert!(fresh.exists());

        let older = aged("older.png");
        drops.prune();
        assert!(!older.exists());
        assert!(fresh.exists());
    }

    #[test]
    fn a_copy_dropped_again_counts_its_week_from_then() {
        let root = tempfile::tempdir().unwrap();
        let drops = drops(root.path());
        let shot = thumbnail(root.path());
        drops.stage(&escaped(&shot)).unwrap();
        let copy = copied(root.path(), "Screenshot-2026-09-21-at-11.13.58-PM.png");
        fs::File::options()
            .write(true)
            .open(&copy)
            .unwrap()
            .set_modified(SystemTime::now() - KEEP + Duration::from_secs(60))
            .unwrap();

        drops.stage(&escaped(&shot)).unwrap();

        let age = fs::metadata(&copy).unwrap().modified().unwrap();
        assert!(SystemTime::now().duration_since(age).unwrap() < Duration::from_secs(60));
    }

    #[test]
    fn a_plain_name_keeps_the_parts_that_read() {
        let name = |name: &str| {
            let (stem, extension) = plain_name(name);
            stem + &extension
        };
        assert_eq!(
            name("Screenshot 2026-09-21 at 11.13.58\u{202f}PM.png"),
            "Screenshot-2026-09-21-at-11.13.58-PM.png"
        );
        assert_eq!(name("shot (1).png"), "shot-1.png");
        assert_eq!(name("截图.png"), "dropped.png");
        assert_eq!(name(".env"), ".env");
        assert_eq!(name("README"), "README");
    }
}
