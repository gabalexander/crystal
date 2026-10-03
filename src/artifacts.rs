//! Artifacts: the files a task keeps as it closes, copied out of its
//! worktree into crystal's state directory, a directory per task.
//!
//! Copied, because a task's results mustn't hang on its worktree: one is
//! removed once its branch is merged, and the next task may run in another.
//! And copied by the daemon: `crystal done --artifact` sends paths only, and
//! the daemon checks each again, that it's a file in the task's worktree,
//! before it reads it. The copies are kept small, [`MAX_FILE_BYTES`] a file
//! and [`MAX_TASK_BYTES`] a task, since they're kept until they're removed
//! by hand: an artifact is for someone to read, not a build's output.

use crate::protocol::{Artifact, ArtifactKind};
use anyhow::{Context, Result, bail};
use std::fs;
use std::path::{Path, PathBuf};

/// The most one kept file may hold.
pub const MAX_FILE_BYTES: u64 = 1024 * 1024;

/// The most a task's files may hold together.
pub const MAX_TASK_BYTES: u64 = 8 * 1024 * 1024;

/// What the worktree's handoff file is kept as. A file of the same name
/// is kept under another.
pub const HANDOFF_NAME: &str = "handoff.md";

/// A file that may be kept: where it is, and how big it was.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checked {
    pub source: PathBuf,
    pub bytes: u64,
}

/// Checks each of `paths` before anything is kept or closed: it must be
/// absolute, a file and not a link to one or a directory, in `worktree`
/// once `..` and links above it are followed, and no bigger than
/// [`MAX_FILE_BYTES`]; together no bigger than [`MAX_TASK_BYTES`]. The
/// first that isn't says so, for the agent that named it.
pub fn check(paths: &[PathBuf], worktree: &Path) -> Result<Vec<Checked>> {
    let root = fs::canonicalize(worktree)
        .with_context(|| format!("the task's worktree, {}, is gone", worktree.display()))?;
    let mut checked = Vec::with_capacity(paths.len());
    let mut total = 0;
    for path in paths {
        let shown = path.display();
        if !path.is_absolute() {
            bail!("{shown} isn't an absolute path; the task is still open");
        }
        let meta = fs::symlink_metadata(path)
            .with_context(|| format!("there's no {shown}; the task is still open"))?;
        if meta.file_type().is_symlink() {
            bail!("{shown} is a link: name the file itself; the task is still open");
        }
        if meta.is_dir() {
            bail!("{shown} is a directory: name the files in it; the task is still open");
        }
        if !meta.is_file() {
            bail!("{shown} isn't a file; the task is still open");
        }
        let resolved = fs::canonicalize(path)
            .with_context(|| format!("couldn't follow {shown}; the task is still open"))?;
        if !resolved.starts_with(&root) {
            bail!(
                "{shown} isn't in the task's worktree, {}: only a file there can be kept, so \
                 copy it in first; the task is still open",
                worktree.display()
            );
        }
        if meta.len() > MAX_FILE_BYTES {
            bail!(
                "{shown} is {}, and a kept file can be {} at most: make it smaller; the task \
                 is still open",
                size(meta.len()),
                size(MAX_FILE_BYTES)
            );
        }
        total += meta.len();
        checked.push(Checked {
            source: resolved,
            bytes: meta.len(),
        });
    }
    if total > MAX_TASK_BYTES {
        bail!(
            "the files come to {}, and a task can keep {} at most: name fewer or smaller ones; \
             the task is still open",
            size(total),
            size(MAX_TASK_BYTES)
        );
    }
    Ok(checked)
}

/// Copies `files` into `dir`, each under its own name unless `taken` has
/// it, or another of them took it first: `plan.md`, then `plan-2.md`. A
/// file that has grown past [`MAX_FILE_BYTES`] since it was checked is
/// refused, and then none of them are kept.
pub fn keep(dir: &Path, files: &[Checked], taken: &[String]) -> Result<Vec<Artifact>> {
    if files.is_empty() {
        return Ok(Vec::new());
    }
    fs::create_dir_all(dir).with_context(|| format!("couldn't make {}", dir.display()))?;
    let mut taken = taken.to_vec();
    let mut kept: Vec<Artifact> = Vec::with_capacity(files.len());
    let copied = files.iter().try_for_each(|file| {
        let own = file.source.file_name().map_or("artifact".into(), |name| {
            name.to_string_lossy().into_owned()
        });
        let name = unique_name(&taken, &own);
        let path = dir.join(&name);
        // In the list before the copy, so a copy cut short goes with the rest.
        kept.push(Artifact {
            kind: ArtifactKind::File,
            name: name.clone(),
            path: path.clone(),
            bytes: 0,
        });
        let bytes = fs::copy(&file.source, &path).with_context(|| {
            format!(
                "couldn't copy {} to {}",
                file.source.display(),
                path.display()
            )
        })?;
        if bytes > MAX_FILE_BYTES {
            bail!(
                "{} grew to {} as it was being kept; the task is still open",
                file.source.display(),
                size(bytes)
            );
        }
        kept.last_mut().expect("just added").bytes = bytes;
        taken.push(name);
        Ok(())
    });
    if let Err(err) = copied {
        for artifact in &kept {
            let _ = fs::remove_file(&artifact.path);
        }
        return Err(err);
    }
    Ok(kept)
}

/// Copies the handoff file `handoff`, when there is one, into `dir` as
/// [`HANDOFF_NAME`]: the notes as they were when the task closed.
pub fn keep_handoff(dir: &Path, handoff: &Path) -> Result<Option<Artifact>> {
    if !fs::symlink_metadata(handoff).is_ok_and(|meta| meta.is_file()) {
        return Ok(None);
    }
    fs::create_dir_all(dir).with_context(|| format!("couldn't make {}", dir.display()))?;
    let path = dir.join(HANDOFF_NAME);
    let bytes = fs::copy(handoff, &path)
        .with_context(|| format!("couldn't copy {} to {}", handoff.display(), path.display()))?;
    Ok(Some(Artifact {
        kind: ArtifactKind::Handoff,
        name: HANDOFF_NAME.to_string(),
        path,
        bytes,
    }))
}

/// `name`, or when `taken` has it, the first of `name-2.ext`, `name-3.ext`…
/// that's free.
fn unique_name(taken: &[String], name: &str) -> String {
    if !taken.iter().any(|other| other == name) {
        return name.to_string();
    }
    // At the last dot that doesn't start the name: `.env` is all stem.
    let (stem, extension) = match name.rfind('.') {
        Some(at) if at > 0 => name.split_at(at),
        _ => (name, ""),
    };
    (2..)
        .map(|number| format!("{stem}-{number}{extension}"))
        .find(|candidate| !taken.contains(candidate))
        .expect("some number is free")
}

/// A size the way a person reads it: `1.5 MiB`, `300 KiB`, `12 bytes`.
pub fn size(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * KIB;
    if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{} KiB", bytes.div_ceil(KIB))
    } else {
        format!("{bytes} bytes")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file of `bytes` that takes no room: only its size is read.
    fn file_of(path: &Path, bytes: u64) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::File::create(path).unwrap().set_len(bytes).unwrap();
    }

    #[test]
    fn only_files_in_the_worktree_and_small_enough_are_kept() {
        let dir = tempfile::tempdir().unwrap();
        let worktree = dir.path().join("app");
        let plan = worktree.join("docs/plan.md");
        file_of(&plan, 10);
        let outside = dir.path().join("elsewhere.md");
        file_of(&outside, 10);

        let ok = check(std::slice::from_ref(&plan), &worktree).unwrap();
        assert_eq!(ok.len(), 1);
        assert_eq!(ok[0].bytes, 10);
        assert!(ok[0].source.ends_with("docs/plan.md"));

        let refused =
            |path: &Path| format!("{:#}", check(&[path.to_path_buf()], &worktree).unwrap_err());
        assert!(refused(&outside).contains("isn't in the task's worktree"));
        let link = worktree.join("link.md");
        std::os::unix::fs::symlink(&plan, &link).unwrap();
        assert!(refused(&link).contains("is a link"));
        assert!(refused(&worktree.join("docs")).contains("is a directory"));
        assert!(refused(&worktree.join("nope.md")).contains("there's no"));
        assert!(refused(Path::new("docs/plan.md")).contains("isn't an absolute path"));
        // `..` that climbs out is followed before it's judged.
        assert!(refused(&worktree.join("docs/../../elsewhere.md")).contains("isn't in"));
        assert!(refused(&outside).ends_with("the task is still open"));

        let big = worktree.join("big.bin");
        file_of(&big, MAX_FILE_BYTES + 1);
        assert!(
            refused(&big).contains("can be 1.0 MiB at most"),
            "{}",
            refused(&big)
        );
        fs::remove_file(&big).unwrap();

        // Nine files of 1 MiB: each is fine, all of them too much.
        let many: Vec<PathBuf> = (0..9)
            .map(|part| {
                let path = worktree.join(format!("part-{part}.bin"));
                file_of(&path, MAX_FILE_BYTES);
                path
            })
            .collect();
        let over = format!("{:#}", check(&many, &worktree).unwrap_err());
        assert!(
            over.contains("come to 9.0 MiB") && over.contains("8.0 MiB at most"),
            "{over}"
        );
        assert_eq!(
            check(&many[..8], &worktree).unwrap().len(),
            8,
            "just enough"
        );
    }

    #[test]
    fn two_files_of_one_name_are_both_kept_beside_the_handoff() {
        let dir = tempfile::tempdir().unwrap();
        let worktree = dir.path().join("app");
        let first = worktree.join("a/plan.md");
        let second = worktree.join("b/plan.md");
        let own_handoff = worktree.join("handoff.md");
        for (path, text) in [
            (&first, "first"),
            (&second, "second"),
            (&own_handoff, "mine"),
        ] {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        let kept_in = dir.path().join("tasks/t1");
        let files = check(&[first, second, own_handoff], &worktree).unwrap();
        let kept = keep(&kept_in, &files, &[HANDOFF_NAME.to_string()]).unwrap();
        let names: Vec<&str> = kept.iter().map(|artifact| artifact.name.as_str()).collect();
        assert_eq!(names, ["plan.md", "plan-2.md", "handoff-2.md"]);
        assert_eq!(
            fs::read_to_string(kept_in.join("plan-2.md")).unwrap(),
            "second"
        );
        assert_eq!(kept[1].bytes, 6);
        assert!(
            kept.iter()
                .all(|artifact| artifact.kind == ArtifactKind::File)
        );

        let notes = worktree.join(".crystal/handoff.md");
        assert_eq!(keep_handoff(&kept_in, &notes).unwrap(), None);
        fs::create_dir_all(notes.parent().unwrap()).unwrap();
        fs::write(&notes, "## 2026 · a\nnote\n").unwrap();
        let handoff = keep_handoff(&kept_in, &notes).unwrap().unwrap();
        assert_eq!(handoff.kind, ArtifactKind::Handoff);
        assert_eq!(handoff.path, kept_in.join("handoff.md"));
        assert!(fs::read_to_string(&handoff.path).unwrap().contains("note"));
    }

    #[test]
    fn a_taken_name_gets_a_number_before_its_extension() {
        let taken = |names: &[&str]| {
            names
                .iter()
                .map(|name| name.to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(unique_name(&taken(&[]), "plan.md"), "plan.md");
        assert_eq!(
            unique_name(&taken(&["plan.md", "plan-2.md"]), "plan.md"),
            "plan-3.md"
        );
        assert_eq!(unique_name(&taken(&["notes"]), "notes"), "notes-2");
        assert_eq!(unique_name(&taken(&[".env"]), ".env"), ".env-2");
    }

    #[test]
    fn a_size_reads_as_a_person_would_say_it() {
        assert_eq!(size(12), "12 bytes");
        assert_eq!(size(300 * 1024), "300 KiB");
        assert_eq!(size(1536 * 1024), "1.5 MiB");
    }
}
