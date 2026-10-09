//! What the syntactic tier read, kept between builds by each file's blob:
//! `syntax.json` in the index's cache directory. A build reads from git
//! only the blobs it doesn't have, parses them on a few threads, and
//! writes the file again with what the commit has, so what no file has any
//! more goes. What the precise tier kept is beside it ([`super::precise`]).

use super::files::{self, Entry, Lang};
use super::grammar::{self, Outline};
use super::keywords;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;

/// The cache's file.
const FILE: &str = "syntax.json";

/// The version of what's kept, which changes whenever what the grammars or
/// the keywords read does; another is read again.
const VERSION: u32 = 1;

#[derive(Debug, Default, Serialize, Deserialize)]
struct Kept {
    version: u32,
    /// Each blob's outline, by `<blob>:<language>`.
    outlines: HashMap<String, Outline>,
}

/// The key a file's outline is kept by: its blob, and the language it's
/// read as, which its name says.
fn key(entry: &Entry, lang: Lang) -> String {
    format!("{}:{lang:?}", entry.blob)
}

/// What each of `read` defines, by its path: what `cache` kept of its blob,
/// or what its grammar or its keywords read in it now. A file in a language
/// with neither, or one that isn't text, has none.
pub fn outlines(root: &Path, cache: &Path, read: &[&Entry]) -> Result<HashMap<String, Outline>> {
    let path = cache.join(FILE);
    let mut kept: Kept = fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .filter(|kept: &Kept| kept.version == VERSION)
        .unwrap_or_default();
    let readable: Vec<(&Entry, Lang)> = (read.iter())
        .filter_map(|entry| Some((*entry, entry.lang?)))
        .filter(|(_, lang)| grammar::compiled(*lang).is_some() || keywords::reads(*lang))
        .collect();
    let missing: Vec<(&Entry, Lang)> = (readable.iter())
        .filter(|(entry, lang)| !kept.outlines.contains_key(&key(entry, *lang)))
        .copied()
        .collect();
    let parsed = parse(root, &missing)?;
    let mut fresh: HashMap<String, Outline> = HashMap::new();
    for ((entry, lang), outline) in missing.iter().zip(parsed) {
        // A file that isn't text is kept as having nothing, so it isn't
        // read again.
        fresh.insert(key(entry, *lang), outline.unwrap_or_default());
    }
    let mut outlines = HashMap::new();
    let mut keep = HashMap::new();
    for (entry, lang) in &readable {
        let key = key(entry, *lang);
        let Some(outline) = fresh.remove(&key).or_else(|| kept.outlines.remove(&key)) else {
            continue;
        };
        if !outline.defs.is_empty() || outline.lines > 0 {
            outlines.insert(entry.path.clone(), outline.clone());
        }
        keep.insert(key, outline);
    }
    let kept = Kept {
        version: VERSION,
        outlines: keep,
    };
    let partial = cache.join(format!("{FILE}.part"));
    if fs::write(&partial, serde_json::to_vec(&kept)?).is_ok() {
        let _ = fs::rename(&partial, &path);
    }
    Ok(outlines)
}

/// What each of `files` defines, read from git and parsed on a few
/// threads, in their order; `None` for one that isn't text.
fn parse(root: &Path, files: &[(&Entry, Lang)]) -> Result<Vec<Option<Outline>>> {
    if files.is_empty() {
        return Ok(Vec::new());
    }
    let workers = thread::available_parallelism()
        .map_or(2, |count| count.get())
        .clamp(1, 8);
    let blobs: Vec<String> = files.iter().map(|(entry, _)| entry.blob.clone()).collect();
    let langs: Vec<Lang> = files.iter().map(|(_, lang)| *lang).collect();
    let results: Mutex<Vec<Option<Outline>>> = Mutex::new(vec![None; files.len()]);
    thread::scope(|scope| -> Result<()> {
        // A few blobs ahead of the workers at most, so a big repository
        // isn't held whole.
        let (send, receive) = mpsc::sync_channel::<(usize, Vec<u8>)>(workers * 4);
        let receive = Arc::new(Mutex::new(receive));
        for _ in 0..workers {
            let receive = Arc::clone(&receive);
            let (results, langs) = (&results, &langs);
            scope.spawn(move || {
                let mut parser = tree_sitter::Parser::new();
                loop {
                    let next = receive.lock().map(|receive| receive.recv());
                    let Ok(Ok((at, content))) = next else { break };
                    let outline = read(langs[at], &content, &mut parser);
                    if let Ok(mut results) = results.lock() {
                        results[at] = outline;
                    }
                }
            });
        }
        let read = files::each_blob(root, &blobs, |at, content| {
            let _ = send.send((at, content));
        });
        drop(send);
        read
    })?;
    Ok(results.into_inner().unwrap_or_default())
}

/// What `content`, in `lang`, defines, by its grammar or else its
/// keywords; `None` when it isn't text.
pub fn read(lang: Lang, content: &[u8], parser: &mut tree_sitter::Parser) -> Option<Outline> {
    let head = &content[..content.len().min(8192)];
    if head.contains(&0)
        || std::str::from_utf8(head).is_err()
            && String::from_utf8_lossy(head).matches('\u{fffd}').count() > 8
    {
        return None;
    }
    grammar::outline(lang, content, parser).or_else(|| keywords::outline(lang, content))
}
