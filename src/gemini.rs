//! Search by meaning through Google's Gemini API, for memory, in place of
//! the model [`crate::embed`] runs on this machine: `embedder = "gemini"`
//! under `[memory]`. Its model, `gemini-embedding-2` unless `gemini_model`
//! says another, turns an entry or a search into a vector of
//! `gemini_dimensions` numbers: 3072, or 1536 or 768, which are the first of
//! the 3072 scaled to length one again. Entries go a hundred to a request,
//! the most Google takes, four requests at once. The reranker crystal runs
//! still reads the best of each search, and the model on this machine is
//! what a search falls back on.
//!
//! What goes to Google: every entry's text, credentials already taken out
//! as memory keeps it, each search, the first prompt of each agent that
//! starts, which what it's shown of the memory is found by, and the last
//! things a session said that the distiller holds against the entries; each
//! with credentials taken out again on the way.
//!
//! The key is the first line of the file `gemini_key_file` names
//! (`gemini.key` beside the config file unless it names another), or else
//! `GEMINI_API_KEY`, or `GOOGLE_API_KEY`. It's read for each request, so a
//! new one counts at once, and given to `curl` on its standard input with
//! the request: never on its command line, where `ps` shows it, never in the
//! config file, which `crystal config export` bundles, and never in what
//! crystal says or logs.
//!
//! A request that fails (offline, out of quota, a key Google refuses) is
//! said once and kept for the status, and none is tried for a while after,
//! as long as Google asks after a 429, so a search meanwhile goes on at
//! once by the local model, or by words.

use crate::config::MemorySettings;
use crate::output::errln;
use crate::{printable, secrets, shell};
use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::fmt;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

/// The model, unless `gemini_model` says another.
pub const MODEL: &str = "gemini-embedding-2";

/// The sizes of vector `gemini_dimensions` takes: those its thresholds
/// were measured at.
pub const DIMENSIONS: [u32; 3] = [768, 1536, 3072];

/// The size unless `gemini_dimensions` says another.
pub const DEFAULT_DIMENSIONS: u32 = 768;

/// What Google asks for a million tokens of text sent to
/// `gemini-embedding-2`, in US dollars, as its price list said in October
/// 2026: half that through its Batch API, which takes up to a day.
const USD_PER_MILLION_TOKENS: f64 = 0.20;

/// Where the API is, unless `CRYSTAL_GEMINI_URL` says, as crystal's tests do.
const API: &str = "https://generativelanguage.googleapis.com/v1beta";

/// The variables the key is read from, in turn, when its file has none.
const KEY_VARIABLES: [&str; 2] = ["GEMINI_API_KEY", "GOOGLE_API_KEY"];

/// The most texts one `batchEmbedContents` takes.
pub const BATCH: usize = 100;

/// The most bytes of text one request carries, well within the line of
/// its config `curl` reads it from.
const BATCH_BYTES: usize = 2 << 20;

/// How many requests go at once, embedding many entries.
pub const AT_ONCE: usize = 4;

/// The most of a text that's sent: the model reads 8,192 tokens of one,
/// and Google counts every token it's sent.
const MAX_TEXT_BYTES: usize = 32 << 10;

/// What goes ahead of a search and of an entry, as Google says the model
/// takes them: a search as a question to answer, which ranked crystal's
/// notes a little better than as a search for results (MRR@10 0.813
/// against 0.783 at 3072, 0.801 against 0.785 at 768).
const QUERY: &str = "task: question answering | query: ";
const DOCUMENT: &str = "title: none | text: ";

/// How long a search's vector is kept, and how many are: agents ask the
/// same again, and each costs a request.
const QUERY_KEPT_FOR: Duration = Duration::from_secs(3600);
const QUERIES_KEPT: usize = 256;

/// How long `curl` may take to connect, and then a search's request, which
/// a search waits on, and a batch's, in seconds.
const CONNECT_TIMEOUT: u64 = 5;
const QUERY_TIMEOUT: u64 = 10;
const BATCH_TIMEOUT: u64 = 60;

/// How long no request is tried after one failed: after the network or
/// Google failing, after a 429 that doesn't say, and the most one that
/// does may ask; and after Google refused the request or the key, unless
/// the key changes.
const REST_OFFLINE: Duration = Duration::from_secs(30);
const REST_QUOTA: Duration = Duration::from_secs(60);
const REST_MOST: Duration = Duration::from_secs(600);
const REST_REFUSED: Duration = Duration::from_secs(300);

/// The thresholds of [`crate::embed::Embed`] tied to how alike the model
/// finds two texts, measured for it on crystal's own memory (512 entries,
/// 2026-10-05), as jina's were. Its cosines sit higher and closer together
/// than jina's: two entries are 0.63 alike in the middle, where jina's are
/// 0.31.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Thresholds {
    /// Nothing is left out for its score alone: the best match for a
    /// question nothing answered scored 0.60 to 0.63, for one something
    /// did, 0.64 at least, too close to cut between; the reranker tells them
    /// apart.
    pub min_similarity: f32,
    /// How far below the best another may score: 0.08 ranked crystal's
    /// notes best, out of 0.03 to 0.11, at both 3072 and 768, as it did for
    /// jina.
    pub near_best: f32,
    /// As alike as this, two entries say the same thing, the reranker or
    /// not: different lessons on one subject were alike up to 0.913 (0.915
    /// at 768), where jina's were up to 0.896.
    pub same_from: f32,
    /// As alike as this, the reranker is asked whether they say the same:
    /// below it, different lessons on one subject scored as high with it as
    /// the same said again, many more of them than with jina, so the band
    /// it decides in starts just above the most alike of them.
    pub alike_from: f32,
}

/// The thresholds at a size of vector: a smaller one's cosines are a
/// little higher.
pub fn thresholds(dimensions: u32) -> Thresholds {
    match dimensions {
        768 => Thresholds {
            min_similarity: 0.0,
            near_best: 0.08,
            same_from: 0.925,
            alike_from: 0.918,
        },
        _ => Thresholds {
            min_similarity: 0.0,
            near_best: 0.08,
            same_from: 0.92,
            alike_from: 0.915,
        },
    }
}

/// Where the key was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyFrom {
    File(PathBuf),
    Variable(&'static str),
}

impl fmt::Display for KeyFrom {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            KeyFrom::File(path) => f.write_str(&shell::home_relative(path)),
            KeyFrom::Variable(name) => write!(f, "${name}"),
        }
    }
}

/// The key, and where it was: the first line of `file`, or else the first
/// of [`KEY_VARIABLES`] `variable` gives, `None` with neither. One with a
/// character no key has is refused, without saying it.
fn key_from(
    file: &Path,
    variable: impl Fn(&str) -> Option<String>,
) -> Result<Option<(String, KeyFrom)>> {
    let found = match std::fs::read_to_string(file) {
        Ok(text) => Some((first_line(&text), KeyFrom::File(file.to_path_buf()))),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => {
            return Err(anyhow!(
                "couldn't read the key in {}: {err}",
                shell::home_relative(file)
            ));
        }
    };
    let found = found.filter(|(key, _)| !key.is_empty()).or_else(|| {
        KEY_VARIABLES.iter().find_map(|name| {
            let key = first_line(&variable(name)?);
            (!key.is_empty()).then_some((key, KeyFrom::Variable(name)))
        })
    });
    if let Some((key, from)) = &found
        && !key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err(anyhow!(
            "the key in {from} has characters no key has: only letters, digits, `-`, `_` and `.`"
        ));
    }
    Ok(found)
}

fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or("").trim().to_string()
}

/// Whether others than its owner can read `file`.
fn shared_file(file: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(file).is_ok_and(|meta| meta.permissions().mode() & 0o077 != 0)
}

/// How the Gemini API stands for memory, as the daemon says it does.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[cfg_attr(test, schemars(rename = "GeminiStatus"))]
pub struct Status {
    /// The model and its size, as each vector is kept under:
    /// `gemini-embedding-2@768`.
    pub model: String,
    /// Where the key is read from, or `None` with none found.
    pub key: Option<String>,
    /// Whether the key's file can be read by others than its owner.
    pub key_shared: bool,
    /// Why the key can't be had, or why the last request failed, while
    /// none has worked since; and how many seconds ago it failed.
    pub failed: Option<String>,
    pub failed_secs_ago: Option<u64>,
    /// How many tokens Google has counted since the process started.
    pub tokens: u64,
}

/// The Gemini API, as memory asks it for vectors.
pub struct Gemini {
    model: String,
    dimensions: u32,
    /// What each of its vectors is kept under: vectors of two sizes can't be
    /// compared either.
    name: String,
    key_file: PathBuf,
    /// Reads a variable the key may be in: the environment's, but in tests.
    variable: fn(&str) -> Option<String>,
    url: String,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    /// Searches' vectors, with when each was asked for.
    queries: HashMap<String, (Instant, Vec<f32>)>,
    failed: Option<Failure>,
    tokens: u64,
}

/// A request that failed: what's said of it, when, how long until another
/// is tried, and a hash of the key it was tried with, so that a new key is
/// tried at once.
struct Failure {
    said: String,
    at: SystemTime,
    until: Instant,
    key: u64,
}

/// Why a request failed, and how long to wait before another.
#[derive(Debug, PartialEq)]
struct Failed {
    said: String,
    rest: Duration,
}

impl Gemini {
    /// The API as `settings` say, at the URL `CRYSTAL_GEMINI_URL` gives or
    /// Google's.
    pub fn new(settings: &MemorySettings) -> Gemini {
        let url = std::env::var("CRYSTAL_GEMINI_URL")
            .ok()
            .filter(|url| !url.is_empty())
            .unwrap_or_else(|| API.to_string());
        Gemini::at(
            &settings.gemini_model,
            settings.gemini_dimensions,
            settings.gemini_key_file(),
            url,
        )
    }

    fn at(model: &str, dimensions: u32, key_file: PathBuf, url: String) -> Gemini {
        let model = model.trim().trim_start_matches("models/");
        Gemini {
            model: model.to_string(),
            dimensions,
            name: name(model, dimensions),
            key_file,
            variable: |name| std::env::var(name).ok(),
            url: url.trim_end_matches('/').to_string(),
            state: Mutex::new(State::default()),
        }
    }

    /// The key, and where it was found.
    fn key(&self) -> Result<Option<(String, KeyFrom)>> {
        key_from(&self.key_file, self.variable)
    }

    /// What its vectors are kept under.
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn thresholds(&self) -> Thresholds {
        thresholds(self.dimensions)
    }

    /// Each entry's vector.
    pub fn embed_documents(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        let texts = texts.iter().map(|text| sent(DOCUMENT, text)).collect();
        self.embed(texts, BATCH_TIMEOUT)
    }

    /// A search's vector: the one asked for in the last hour, if it was.
    pub fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        let now = Instant::now();
        {
            let mut state = self.state.lock().unwrap();
            state
                .queries
                .retain(|_, (asked, _)| now.duration_since(*asked) < QUERY_KEPT_FOR);
            if let Some((_, vector)) = state.queries.get(text) {
                return Ok(vector.clone());
            }
        }
        let vector = self
            .embed(vec![sent(QUERY, text)], QUERY_TIMEOUT)?
            .pop()
            .context("Gemini gave no vector")?;
        let mut state = self.state.lock().unwrap();
        if state.queries.len() >= QUERIES_KEPT
            && let Some(oldest) = state
                .queries
                .iter()
                .min_by_key(|(_, (asked, _))| *asked)
                .map(|(query, _)| query.clone())
        {
            state.queries.remove(&oldest);
        }
        state
            .queries
            .insert(text.to_string(), (now, vector.clone()));
        Ok(vector)
    }

    /// How it stands: where the key is, and the last failure since nothing
    /// worked.
    pub fn status(&self) -> Status {
        let (key, failed) = match self.key() {
            Ok(Some((_, from))) => (Some(from), None),
            Ok(None) => (None, Some(no_key(&self.key_file))),
            Err(err) => (None, Some(format!("{err:#}"))),
        };
        let state = self.state.lock().unwrap();
        let last = state.failed.as_ref();
        Status {
            model: self.name.clone(),
            key_shared: matches!(&key, Some(KeyFrom::File(file)) if shared_file(file)),
            key: key.map(|from| from.to_string()),
            failed: failed.or_else(|| last.map(|failure| failure.said.clone())),
            failed_secs_ago: last.and_then(|failure| {
                Some(SystemTime::now().duration_since(failure.at).ok()?.as_secs())
            }),
            tokens: state.tokens,
        }
    }

    /// Whether a request would be tried now: there's a key, and none failed
    /// lately with it.
    pub fn ready(&self) -> bool {
        let Ok(Some((key, _))) = self.key() else {
            return false;
        };
        let state = self.state.lock().unwrap();
        !state
            .failed
            .as_ref()
            .is_some_and(|failure| failure.key == hash(&key) && Instant::now() < failure.until)
    }

    /// The vectors of `texts`, as they're to be sent, in as many requests
    /// as they take, [`AT_ONCE`] at a time.
    fn embed(&self, texts: Vec<String>, timeout: u64) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let key = match self.key() {
            Ok(Some((key, _))) => key,
            Ok(None) => return Err(anyhow!(no_key(&self.key_file))),
            Err(err) => return Err(err),
        };
        let tried_with = hash(&key);
        if let Some(failure) = &self.state.lock().unwrap().failed
            && failure.key == tried_with
            && Instant::now() < failure.until
        {
            return Err(anyhow!("{} (not asked again yet)", failure.said));
        }
        let batches = batches(&texts);
        let mut vectors = Vec::with_capacity(texts.len());
        let mut tokens = 0;
        let mut failed = None;
        for wave in batches.chunks(AT_ONCE) {
            let answers: Vec<_> = std::thread::scope(|scope| {
                let asked: Vec<_> = wave
                    .iter()
                    .map(|batch| scope.spawn(|| self.request(&key, batch, timeout)))
                    .collect();
                asked
                    .into_iter()
                    .map(|asked| asked.join().expect("a request to Gemini panicked"))
                    .collect()
            });
            for answer in answers {
                match answer {
                    Ok((got, counted)) => {
                        vectors.extend(got);
                        tokens += counted;
                    }
                    Err(err) => failed = failed.or(Some(err)),
                }
            }
            if failed.is_some() {
                break;
            }
        }
        let mut state = self.state.lock().unwrap();
        state.tokens += tokens;
        match failed {
            None => {
                state.failed = None;
                Ok(vectors)
            }
            Some(Failed { said, rest }) => {
                let said = format!("{}: {}", self.model, scrubbed(&said, &key));
                if state
                    .failed
                    .as_ref()
                    .is_none_or(|before| before.said != said)
                {
                    errln!("crystal: couldn't ask Gemini for vectors: {said}");
                }
                state.failed = Some(Failure {
                    said: said.clone(),
                    at: SystemTime::now(),
                    until: Instant::now() + rest,
                    key: tried_with,
                });
                Err(anyhow!(said))
            }
        }
    }

    /// One `batchEmbedContents` of `texts`, with `key`: their vectors, and
    /// how many tokens Google counted.
    fn request(
        &self,
        key: &str,
        texts: &[String],
        timeout: u64,
    ) -> Result<(Vec<Vec<f32>>, u64), Failed> {
        let model = format!("models/{}", self.model);
        let requests: Vec<_> = texts
            .iter()
            .map(|text| {
                json!({
                    "model": model,
                    "content": {"parts": [{"text": text}]},
                    "embedContentConfig": {"outputDimensionality": self.dimensions},
                })
            })
            .collect();
        let body = json!({ "requests": requests }).to_string();
        let url = format!("{}/{model}:batchEmbedContents", self.url);
        let (code, reply) = post(&url, key, &body, timeout)?;
        read_reply(code, &reply, texts.len(), self.dimensions as usize)
    }
}

/// What `tokens` cost, as `gemini-embedding-2`'s price list has it.
pub fn cost(tokens: u64) -> String {
    format!(
        "about ${:.4} at gemini-embedding-2's price",
        tokens as f64 * USD_PER_MILLION_TOKENS / 1e6
    )
}

/// What a model's vectors at a size are kept under.
pub fn name(model: &str, dimensions: u32) -> String {
    format!("{model}@{dimensions}")
}

/// What's said when there's no key.
fn no_key(file: &Path) -> String {
    format!(
        "no key for Gemini: put it in {}, or set GEMINI_API_KEY",
        shell::home_relative(file)
    )
}

/// `text` as it's sent: credentials taken out, cut to what the model
/// reads, after `task`.
fn sent(task: &str, text: &str) -> String {
    let text = secrets::redact(text);
    let mut end = text.len().min(MAX_TEXT_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{task}{}", &text[..end])
}

/// `texts` in batches a request takes: [`BATCH`] texts at most, and
/// [`BATCH_BYTES`] of them, but for one alone that's more.
fn batches(texts: &[String]) -> Vec<&[String]> {
    let mut batches = Vec::new();
    let (mut start, mut bytes) = (0, 0);
    for (at, text) in texts.iter().enumerate() {
        let full = at - start == BATCH || (at > start && bytes + text.len() > BATCH_BYTES);
        if full {
            batches.push(&texts[start..at]);
            (start, bytes) = (at, 0);
        }
        bytes += text.len();
    }
    batches.push(&texts[start..]);
    batches
}

/// A hash of the key, to tell it from another without keeping it.
fn hash(key: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    hasher.finish()
}

/// `said` made fit to show and keep: one line, not too long, and nothing in
/// it that could be the key or another credential.
fn scrubbed(said: &str, key: &str) -> String {
    let said = said.replace(key, secrets::REDACTED);
    let said = secrets::redact(&printable::line(&said));
    let mut said: String = said.split_whitespace().collect::<Vec<_>>().join(" ");
    if said.chars().count() > 240 {
        said = said.chars().take(239).collect::<String>() + "…";
    }
    said
}

/// What `curl` reads as its config: the key in a header, and the request.
/// What's in quotes takes `\` before `"` and `\`, and writes a line's end
/// and a tab as `\n`, `\r` and `\t`.
fn curl_config(key: &str, body: &str) -> String {
    let mut quoted = String::with_capacity(body.len() + 16);
    for c in body.chars() {
        match c {
            '\\' => quoted.push_str("\\\\"),
            '"' => quoted.push_str("\\\""),
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            '\t' => quoted.push_str("\\t"),
            c => quoted.push(c),
        }
    }
    format!(
        "header = \"x-goog-api-key: {key}\"\nheader = \"Content-Type: application/json\"\n\
         data-binary = \"{quoted}\"\n"
    )
}

/// POSTs `body` to `url` with `curl`, the key and the body given on its
/// standard input: the HTTP status and what came back.
fn post(url: &str, key: &str, body: &str, timeout: u64) -> Result<(u16, String), Failed> {
    let offline = |said: String| Failed {
        said,
        rest: REST_OFFLINE,
    };
    let mut curl = Command::new("curl")
        .args(["--silent", "--show-error", "--config", "-"])
        .args(["--connect-timeout", &CONNECT_TIMEOUT.to_string()])
        .args(["--max-time", &timeout.to_string()])
        .args(["--write-out", "\n%{http_code}"])
        .arg(url)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| offline(format!("couldn't run curl, which asks it: {err}")))?;
    let config = curl_config(key, body);
    let mut stdin = curl.stdin.take().expect("curl's standard input is piped");
    // Given on a thread of its own, so a curl that gives up reading it
    // early can be heard out.
    let giving = std::thread::spawn(move || stdin.write_all(config.as_bytes()));
    let output = curl
        .wait_with_output()
        .map_err(|err| offline(format!("curl failed: {err}")))?;
    let _ = giving.join();
    let out = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() {
        let said = String::from_utf8_lossy(&output.stderr);
        return Err(offline(
            said.trim().trim_start_matches("curl: ").to_string(),
        ));
    }
    let (reply, code) = out.rsplit_once('\n').unwrap_or(("", &out));
    let code = code
        .trim()
        .parse()
        .map_err(|_| offline(format!("curl gave no HTTP status: {code}")))?;
    Ok((code, reply.to_string()))
}

/// What `batchEmbedContents` answers.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Reply {
    #[serde(default)]
    embeddings: Vec<Embedding>,
    #[serde(default)]
    usage_metadata: Option<Usage>,
}

#[derive(Deserialize)]
struct Embedding {
    values: Vec<f32>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Usage {
    #[serde(default)]
    prompt_token_count: u64,
}

/// What Google answers a request it doesn't carry out.
#[derive(Deserialize)]
struct Refusal {
    error: RefusalError,
}

#[derive(Deserialize)]
struct RefusalError {
    #[serde(default)]
    message: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    details: Vec<serde_json::Value>,
}

/// The vectors in a reply with HTTP status `code` to a request of `count`
/// texts, each of `dimensions` numbers, scaled to length one, and how many
/// tokens it counted; or why it failed and how long to rest, as long as a
/// 429 asks, within reason.
fn read_reply(
    code: u16,
    reply: &str,
    count: usize,
    dimensions: usize,
) -> Result<(Vec<Vec<f32>>, u64), Failed> {
    if code != 200 {
        let refusal = serde_json::from_str::<Refusal>(reply).ok();
        let rest = match code {
            429 => refusal
                .as_ref()
                .and_then(|refusal| retry_delay(&refusal.error.details))
                .unwrap_or(REST_QUOTA)
                .clamp(Duration::from_secs(5), REST_MOST),
            500.. => REST_OFFLINE,
            _ => REST_REFUSED,
        };
        let said = match refusal {
            Some(Refusal { error }) if !error.message.is_empty() => {
                let status = Some(error.status).filter(|status| !status.is_empty());
                format!(
                    "{} ({})",
                    error.message.trim(),
                    status.unwrap_or_else(|| code.to_string())
                )
            }
            _ => format!("HTTP {code}"),
        };
        return Err(Failed { said, rest });
    }
    let unreadable = |what: String| Failed {
        said: what,
        rest: REST_REFUSED,
    };
    let reply: Reply = serde_json::from_str(reply)
        .map_err(|err| unreadable(format!("couldn't read its answer: {err}")))?;
    if reply.embeddings.len() != count {
        return Err(unreadable(format!(
            "it gave {} vectors for {count} texts",
            reply.embeddings.len()
        )));
    }
    let mut vectors = Vec::with_capacity(count);
    for Embedding { values } in reply.embeddings {
        if values.len() != dimensions {
            return Err(unreadable(format!(
                "it gave a vector of {} numbers, not {dimensions}",
                values.len()
            )));
        }
        let length = values.iter().map(|x| x * x).sum::<f32>().sqrt();
        if !length.is_finite() || length == 0.0 {
            return Err(unreadable("it gave a vector of no length".to_string()));
        }
        vectors.push(values.iter().map(|x| x / length).collect());
    }
    let tokens = reply
        .usage_metadata
        .map_or(0, |usage| usage.prompt_token_count);
    Ok((vectors, tokens))
}

/// How long Google's `RetryInfo` asks to wait, as `"37s"` or `"1.5s"`.
fn retry_delay(details: &[serde_json::Value]) -> Option<Duration> {
    details.iter().find_map(|detail| {
        let delay = detail.get("retryDelay")?.as_str()?;
        let seconds: f64 = delay.strip_suffix('s')?.parse().ok()?;
        (seconds.is_finite() && seconds >= 0.0).then(|| Duration::from_secs_f64(seconds))
    })
}

/// The API, made once in each process for the settings as they are and
/// kept, with the searches it was asked and how it last failed; made
/// again when they change.
pub fn shared(settings: &MemorySettings) -> Arc<Gemini> {
    static SHARED: Mutex<Option<Arc<Gemini>>> = Mutex::new(None);
    let wanted = Gemini::new(settings);
    let mut shared = SHARED.lock().unwrap();
    if let Some(gemini) = &*shared
        && gemini.name == wanted.name
        && gemini.key_file == wanted.key_file
        && gemini.url == wanted.url
    {
        return gemini.clone();
    }
    let gemini = Arc::new(wanted);
    *shared = Some(gemini.clone());
    gemini
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read};
    use std::net::TcpListener;

    #[test]
    fn the_key_is_read_from_its_file_then_the_environment_and_checked() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("gemini.key");
        let none = |_: &str| None;
        assert_eq!(key_from(&file, none).unwrap(), None);
        let from_google = |name: &str| (name == "GOOGLE_API_KEY").then(|| "AQ.fake-2".to_string());
        assert_eq!(
            key_from(&file, from_google).unwrap(),
            Some(("AQ.fake-2".into(), KeyFrom::Variable("GOOGLE_API_KEY")))
        );
        let both = |name: &str| Some(format!("{name}-fake"));
        assert_eq!(
            key_from(&file, both).unwrap().unwrap().1,
            KeyFrom::Variable("GEMINI_API_KEY")
        );
        std::fs::write(&file, "  AQ.fake_key-1\nignored\n").unwrap();
        assert_eq!(
            key_from(&file, both).unwrap(),
            Some(("AQ.fake_key-1".into(), KeyFrom::File(file.clone())))
        );
        // An empty file leaves it to the environment.
        std::fs::write(&file, "\n").unwrap();
        assert_eq!(
            key_from(&file, both).unwrap().unwrap().1,
            KeyFrom::Variable("GEMINI_API_KEY")
        );
        // A key that would break out of its header is refused, unsaid.
        std::fs::write(&file, "fake\" secret-x").unwrap();
        let err = format!("{:#}", key_from(&file, none).unwrap_err());
        assert!(err.contains("characters no key has"), "{err}");
        assert!(!err.contains("secret"), "{err}");
        assert_eq!(
            KeyFrom::Variable("GEMINI_API_KEY").to_string(),
            "$GEMINI_API_KEY"
        );
    }

    #[test]
    fn every_size_asks_the_reranker_only_above_the_most_alike_different_lessons() {
        // On crystal's notes, two different lessons on one subject, the
        // numbers of an eval and the pick they led to, were 0.913 alike at
        // 3072 and 1536, and 0.915 at 768.
        for (size, apart) in [(3072, 0.913), (1536, 0.913), (768, 0.915)] {
            let measured = thresholds(size);
            assert!(measured.alike_from > apart, "{size}");
            assert!(measured.same_from > measured.alike_from, "{size}");
        }
    }

    #[test]
    fn curl_reads_the_key_in_a_header_and_the_request_quoted() {
        let body = json!({"text": "say \"hi\" \\ then\nstop\t."}).to_string();
        let config = curl_config("AQ.fake", &body);
        let mut lines = config.lines();
        assert_eq!(lines.next(), Some("header = \"x-goog-api-key: AQ.fake\""));
        assert_eq!(
            lines.next(),
            Some("header = \"Content-Type: application/json\"")
        );
        assert_eq!(
            lines.next(),
            Some(r#"data-binary = "{\"text\":\"say \\\"hi\\\" \\\\ then\\nstop\\t.\"}""#)
        );
        assert_eq!(lines.next(), None);
    }

    #[test]
    fn texts_go_after_their_task_without_credentials_and_cut_to_what_is_read() {
        assert_eq!(
            sent(QUERY, "how do I merge PRs"),
            "task: question answering | query: how do I merge PRs"
        );
        assert_eq!(
            sent(
                DOCUMENT,
                "export OPENAI_API_KEY=sk-fake0123456789abcdefghij"
            ),
            "title: none | text: export OPENAI_API_KEY=[redacted]"
        );
        let long = "é".repeat(MAX_TEXT_BYTES);
        let cut = sent(DOCUMENT, &long);
        assert!(cut.len() <= DOCUMENT.len() + MAX_TEXT_BYTES);
        assert!(cut.ends_with('é'));
    }

    #[test]
    fn texts_are_batched_by_how_many_and_how_long() {
        let texts: Vec<String> = (0..250).map(|n| n.to_string()).collect();
        let sizes: Vec<usize> = batches(&texts).iter().map(|batch| batch.len()).collect();
        assert_eq!(sizes, [100, 100, 50]);
        let big = vec!["x".repeat(BATCH_BYTES / 2 + 1); 3];
        let sizes: Vec<usize> = batches(&big).iter().map(|batch| batch.len()).collect();
        assert_eq!(sizes, [1, 1, 1]);
        let huge = vec!["x".repeat(BATCH_BYTES * 2)];
        assert_eq!(batches(&huge).len(), 1, "one alone goes, however long");
    }

    #[test]
    fn a_reply_gives_its_vectors_at_length_one_or_says_why_it_failed() {
        let reply = r#"{"embeddings": [{"values": [3, 4]}, {"values": [0, 2]}],
                        "usageMetadata": {"promptTokenCount": 12}}"#;
        let (vectors, tokens) = read_reply(200, reply, 2, 2).unwrap();
        assert_eq!(vectors, vec![vec![0.6, 0.8], vec![0.0, 1.0]]);
        assert_eq!(tokens, 12);
        assert!(read_reply(200, reply, 3, 2).is_err(), "one short");
        assert!(read_reply(200, reply, 2, 768).is_err(), "the wrong size");
        let quota = r#"{"error": {"code": 429, "message": "Quota exceeded.",
            "status": "RESOURCE_EXHAUSTED", "details": [
              {"@type": "type.googleapis.com/google.rpc.RetryInfo", "retryDelay": "37s"}]}}"#;
        assert_eq!(
            read_reply(429, quota, 1, 2).unwrap_err(),
            Failed {
                said: "Quota exceeded. (RESOURCE_EXHAUSTED)".into(),
                rest: Duration::from_secs(37)
            }
        );
        let refused = r#"{"error": {"code": 400, "message": "API key not valid.",
            "status": "INVALID_ARGUMENT"}}"#;
        assert_eq!(
            read_reply(400, refused, 1, 2).unwrap_err().rest,
            REST_REFUSED
        );
        let down = read_reply(503, "<html>", 1, 2).unwrap_err();
        assert_eq!(down.said, "HTTP 503");
        assert_eq!(down.rest, REST_OFFLINE);
    }

    #[test]
    fn what_is_said_of_a_failure_never_has_the_key() {
        let said = scrubbed(
            "bad key AQ.fake-key-123\u{7} in\nthe   header",
            "AQ.fake-key-123",
        );
        assert_eq!(said, "bad key [redacted] in the header");
    }

    /// A web server of the test's own that answers `batchEmbedContents` as
    /// `answer` says, given the request's headers and body, and keeps them.
    struct Fake {
        url: String,
        asked: Arc<Mutex<Vec<(String, String)>>>,
    }

    impl Fake {
        fn new(answer: fn(usize, &str) -> (u16, String)) -> Fake {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}/v1beta", listener.local_addr().unwrap());
            let asked = Arc::new(Mutex::new(Vec::new()));
            let kept = asked.clone();
            std::thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut head = String::new();
                    let mut length = 0;
                    loop {
                        let mut line = String::new();
                        if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                            break;
                        }
                        if let Some(value) = line.to_lowercase().strip_prefix("content-length:") {
                            length = value.trim().parse().unwrap();
                        }
                        head.push_str(&line);
                    }
                    let mut body = vec![0; length];
                    reader.read_exact(&mut body).unwrap();
                    let body = String::from_utf8(body).unwrap();
                    let count = serde_json::from_str::<serde_json::Value>(&body)
                        .map_or(0, |body| body["requests"].as_array().map_or(0, Vec::len));
                    let (code, reply) = answer(count, &body);
                    kept.lock().unwrap().push((head, body));
                    let mut stream = stream;
                    let _ = write!(
                        stream,
                        "HTTP/1.1 {code} X\r\nContent-Type: application/json\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                        reply.len()
                    );
                }
            });
            Fake { url, asked }
        }

        fn asked(&self) -> Vec<(String, String)> {
            self.asked.lock().unwrap().clone()
        }
    }

    /// The API at `url`, its key in `key_file` alone.
    fn gemini_at(dimensions: u32, key_file: PathBuf, url: &str) -> Gemini {
        Gemini {
            variable: |_| None,
            ..Gemini::at("gemini-embedding-2", dimensions, key_file, url.to_string())
        }
    }

    /// Two numbers for each text asked: 1 and 0 for a search, 0 and 1 for
    /// an entry.
    fn vectors(count: usize, body: &str) -> (u16, String) {
        let value = if body.contains("task: question answering") {
            "[1, 0]"
        } else {
            "[0, 1]"
        };
        let embeddings = vec![format!("{{\"values\": {value}}}"); count].join(",");
        let reply = format!(
            "{{\"embeddings\": [{embeddings}], \"usageMetadata\": {{\"promptTokenCount\": {count}}}}}"
        );
        (200, reply)
    }

    #[test]
    fn requests_carry_the_key_in_a_header_and_searches_are_asked_once() {
        let fake = Fake::new(vectors);
        let dir = tempfile::tempdir().unwrap();
        let key_file = dir.path().join("gemini.key");
        std::fs::write(&key_file, "AQ.fake-for-tests\n").unwrap();
        let gemini = gemini_at(2, key_file, &fake.url);
        let texts: Vec<String> = (0..250).map(|n| format!("entry {n}")).collect();
        let texts: Vec<&str> = texts.iter().map(String::as_str).collect();
        let vectors = gemini.embed_documents(&texts).unwrap();
        assert_eq!(vectors.len(), 250);
        assert!(vectors.iter().all(|vector| vector == &[0.0, 1.0]));
        assert_eq!(gemini.embed_query("merge PRs").unwrap(), [1.0, 0.0]);
        assert_eq!(gemini.embed_query("merge PRs").unwrap(), [1.0, 0.0]);
        let asked = fake.asked();
        assert_eq!(asked.len(), 4, "three batches and one search");
        for (head, body) in &asked {
            assert!(
                head.starts_with("POST /v1beta/models/gemini-embedding-2:batchEmbedContents "),
                "{head}"
            );
            assert!(
                head.contains("x-goog-api-key: AQ.fake-for-tests\r\n"),
                "{head}"
            );
            assert!(!head.lines().next().unwrap().contains("AQ.fake"), "{head}");
            assert!(body.contains("\"outputDimensionality\":2"), "{body}");
        }
        assert!(
            asked[3]
                .1
                .contains("task: question answering | query: merge PRs")
        );
        let status = gemini.status();
        assert_eq!(status.model, "gemini-embedding-2@2");
        assert_eq!(status.tokens, 251);
        assert_eq!(status.failed, None);
        assert!(status.key.unwrap().ends_with("gemini.key"));
        assert!(gemini.ready());
    }

    #[test]
    fn a_failure_is_kept_and_nothing_asked_until_the_rest_is_over_or_the_key_changes() {
        let fake = Fake::new(|_, _| {
            let quota = r#"{"error": {"code": 429, "message": "Quota exceeded for AQ.fake-1",
                "status": "RESOURCE_EXHAUSTED"}}"#;
            (429, quota.to_string())
        });
        let dir = tempfile::tempdir().unwrap();
        let key_file = dir.path().join("gemini.key");
        std::fs::write(&key_file, "AQ.fake-1").unwrap();
        let gemini = gemini_at(2, key_file.clone(), &fake.url);
        let err = format!("{:#}", gemini.embed_query("one").unwrap_err());
        assert!(err.contains("Quota exceeded"), "{err}");
        assert!(!err.contains("AQ.fake-1"), "{err}");
        assert!(gemini.embed_documents(&["two"]).is_err());
        assert_eq!(fake.asked().len(), 1, "resting after a 429");
        assert!(!gemini.ready());
        let status = gemini.status();
        assert!(status.failed.unwrap().contains("RESOURCE_EXHAUSTED"));
        assert!(status.failed_secs_ago.is_some());
        std::fs::write(&key_file, "AQ.fake-2").unwrap();
        assert!(gemini.ready(), "a new key is tried at once");
        assert!(gemini.embed_query("three").is_err());
        assert_eq!(fake.asked().len(), 2);
        // Nothing listens on a port bound and let go: as offline.
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        let offline = gemini_at(2, key_file, &format!("http://{port}"));
        let err = format!("{:#}", offline.embed_query("four").unwrap_err());
        assert!(err.starts_with("gemini-embedding-2: "), "{err}");
        std::fs::remove_file(dir.path().join("gemini.key")).unwrap();
        let unkeyed = gemini_at(2, dir.path().join("gemini.key"), &fake.url);
        let status = unkeyed.status();
        assert_eq!(status.key, None);
        assert!(status.failed.unwrap().starts_with("no key for Gemini"));
    }
}
