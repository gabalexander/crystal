//! Reranking through Voyage AI's API, for memory, in place of the reranker
//! [`crate::embed`] runs on this machine: `reranker = "voyage"` under
//! `[memory]`. Its model, `rerank-3` unless `voyage_model` says another,
//! reads a search with the entries found best for it and scores how well
//! each answers it, from 0 to 1, as jina-reranker-v3 does here; so the
//! reranker takes no memory on this machine, and with Gemini making the
//! vectors ([`crate::gemini`]), nor does anything else.
//!
//! What goes to Voyage AI: each search, the first prompt of each agent that
//! starts among them, which what it's shown of the memory is found by, with
//! the text of the entries found best for it; and an entry being
//! remembered, or deduped, with those most alike to it. Each with
//! credentials taken out on the way.
//!
//! The key is the first line of the file `voyage_key_file` names
//! (`voyage.key` beside the config file unless it names another), or else
//! `VOYAGE_API_KEY`, read and given to `curl` as [`crate::web_api`] says:
//! never on its command line, in the config file or in what crystal says.
//!
//! A request that fails (offline, out of quota, a key Voyage refuses) is
//! said once and kept for the status, and none is tried for a while after,
//! so a search meanwhile goes on at once without a reranker, as it does
//! with `rerank = false`.

use crate::config::MemorySettings;
use crate::output::errln;
use crate::shell;
use crate::web_api::{self, Failed, Failure, KeyFrom, Standing};
use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The model, unless `voyage_model` says another: Voyage's newest, which
/// left the widest gap on crystal's memory between a search something
/// answers and one nothing does, and ranked as the others did.
pub const MODEL: &str = "rerank-3";

/// The models `voyage_model` takes: those its thresholds were measured for.
pub const MODELS: [&str; 3] = ["rerank-3", "rerank-3-lite", "rerank-2.5"];

/// What Voyage asks for a million tokens read by `rerank-3` or
/// `rerank-2.5`, in US dollars, as its price list said in October 2026,
/// after the first 200 million; `rerank-3-lite` costs $0.02.
const USD_PER_MILLION_TOKENS: f64 = 0.05;

/// Where the API is, unless `CRYSTAL_VOYAGE_URL` says, as crystal's tests do.
const API: &str = "https://api.voyageai.com/v1";

/// The variable the key is read from when its file has none.
const KEY_VARIABLES: [&str; 1] = ["VOYAGE_API_KEY"];

/// How long a request may take, in seconds: a search waits on it.
const TIMEOUT: u64 = 10;

/// The most of a search or an entry that's sent: Voyage reads 8,000 tokens
/// of a search and 32,000 of it with an entry, and counts every token it
/// reads.
const MAX_TEXT_BYTES: usize = 16 << 10;

/// How long what a search was scored is kept, and how many are: agents ask
/// the same again, and each costs a request.
const KEPT_FOR: Duration = Duration::from_secs(3600);
const KEPT: usize = 256;

/// The thresholds of [`crate::embed::Embed`] tied to the reranker's
/// scores, measured for each model on crystal's own memory (658 entries,
/// 2026-10-12), each search's 20 entries as jina-reranker-v3 was given
/// them: 49 searches something answered and 12 nothing did, and the 261
/// pairs `crystal memory dedupe` asked the reranker about. Voyage's scores
/// sit between 0 and 1, an entry that has nothing to do with a search at
/// about 0.2 to 0.4, where jina's sit about 0.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Thresholds {
    /// The score the best entry has to reach for a search to find anything:
    /// halfway between the best for a search nothing answered (0.31 at most
    /// with `rerank-3`) and for one something did (0.54 at least).
    pub answers_from: f32,
    /// The score below which an entry is left out, once one answers: as
    /// many kept as with jina's, about 8 of the 20, and every right one.
    pub kept_from: f32,
    /// The score an entry said before has to reach, the one being added
    /// read as the search, for the two to say the same thing: above every
    /// pair of different lessons on one subject (0.867 at most), which
    /// Voyage scores closer to the same said again than jina does.
    pub same_reranked_from: f32,
}

/// The thresholds for `model`.
pub fn thresholds(model: &str) -> Thresholds {
    match model {
        "rerank-3-lite" => Thresholds {
            answers_from: 0.47,
            kept_from: 0.42,
            same_reranked_from: 0.88,
        },
        "rerank-2.5" => Thresholds {
            answers_from: 0.49,
            kept_from: 0.40,
            same_reranked_from: 0.87,
        },
        _ => Thresholds {
            answers_from: 0.42,
            kept_from: 0.40,
            same_reranked_from: 0.87,
        },
    }
}

/// How Voyage AI's API stands for memory, as the daemon says it does.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[cfg_attr(test, schemars(rename = "VoyageStatus"))]
pub struct Status {
    /// The model that reranks: `rerank-3`.
    pub model: String,
    /// Where the key is read from, or `None` with none found.
    pub key: Option<String>,
    /// Whether the key's file can be read by others than its owner.
    pub key_shared: bool,
    /// Why the key can't be had, or why the last request failed, while
    /// none has worked since; and how many seconds ago it failed.
    pub failed: Option<String>,
    pub failed_secs_ago: Option<u64>,
    /// How many tokens Voyage has counted since the process started.
    pub tokens: u64,
}

/// Voyage AI's API, as memory asks it to rerank.
pub struct Voyage {
    model: String,
    key_file: PathBuf,
    /// Reads a variable the key may be in: the environment's, but in tests.
    variable: fn(&str) -> Option<String>,
    url: String,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    /// What each search was scored, by a hash of it and what it read, with
    /// when it was asked.
    scored: HashMap<u64, (Instant, Vec<f32>)>,
    failed: Option<Failure>,
    /// Whether it was said that there's no key, since there was one.
    said_no_key: bool,
    tokens: u64,
}

impl Voyage {
    /// The API as `settings` say, at the URL `CRYSTAL_VOYAGE_URL` gives or
    /// Voyage's.
    pub fn new(settings: &MemorySettings) -> Voyage {
        let url = std::env::var("CRYSTAL_VOYAGE_URL")
            .ok()
            .filter(|url| !url.is_empty())
            .unwrap_or_else(|| API.to_string());
        Voyage::at(&settings.voyage_model, settings.voyage_key_file(), url)
    }

    /// The API at `url`, its key in `key_file` alone, for tests elsewhere.
    #[cfg(test)]
    pub fn for_tests(key_file: PathBuf, url: &str) -> Voyage {
        Voyage {
            variable: |_| None,
            ..Voyage::at(MODEL, key_file, url.to_string())
        }
    }

    fn at(model: &str, key_file: PathBuf, url: String) -> Voyage {
        Voyage {
            model: model.trim().to_string(),
            key_file,
            variable: |name| std::env::var(name).ok(),
            url: url.trim_end_matches('/').to_string(),
            state: Mutex::new(State::default()),
        }
    }

    /// The key, and where it was found.
    fn key(&self) -> Result<Option<(String, KeyFrom)>> {
        web_api::key_from(&self.key_file, &KEY_VARIABLES, self.variable)
    }

    pub fn thresholds(&self) -> Thresholds {
        thresholds(&self.model)
    }

    /// How well each of `passages` answers `query`, in their order: as
    /// Voyage scored them, or as it did in the last hour. A failure is said
    /// once, as it happens.
    pub fn rerank(&self, query: &str, passages: &[&str]) -> Result<Vec<f32>> {
        if passages.is_empty() {
            return Ok(Vec::new());
        }
        let key = match self.key() {
            Ok(Some((key, _))) => {
                self.state.lock().unwrap().said_no_key = false;
                key
            }
            Ok(None) => {
                let said = no_key(&self.key_file);
                let mut state = self.state.lock().unwrap();
                if !state.said_no_key {
                    errln!("crystal: couldn't ask Voyage to rerank: {said}");
                    state.said_no_key = true;
                }
                return Err(anyhow!(said));
            }
            Err(err) => return Err(err),
        };
        let query = web_api::sent(query, MAX_TEXT_BYTES);
        let passages: Vec<String> = (passages.iter())
            .map(|passage| web_api::sent(passage, MAX_TEXT_BYTES))
            .collect();
        let asked = web_api::hash(&format!("{query}\0{}", passages.join("\0")));
        let tried_with = web_api::hash(&key);
        let now = Instant::now();
        {
            let mut state = self.state.lock().unwrap();
            state
                .scored
                .retain(|_, (when, _)| now.duration_since(*when) < KEPT_FOR);
            if let Some((_, scores)) = state.scored.get(&asked) {
                return Ok(scores.clone());
            }
            if let Some(failure) = &state.failed
                && failure.resting(tried_with)
            {
                return Err(anyhow!("{} (not asked again yet)", failure.said));
            }
        }
        let answer = self.request(&key, &query, &passages);
        let mut state = self.state.lock().unwrap();
        match answer {
            Ok((scores, tokens)) => {
                state.tokens += tokens;
                state.failed = None;
                if state.scored.len() >= KEPT
                    && let Some(oldest) = (state.scored.iter())
                        .min_by_key(|(_, (when, _))| *when)
                        .map(|(asked, _)| *asked)
                {
                    state.scored.remove(&oldest);
                }
                state.scored.insert(asked, (now, scores.clone()));
                Ok(scores)
            }
            Err(failed) => {
                let said = format!("{}: {}", self.model, web_api::scrubbed(&failed.said, &key));
                if state
                    .failed
                    .as_ref()
                    .is_none_or(|before| before.said != said)
                {
                    errln!("crystal: couldn't ask Voyage to rerank: {said}");
                }
                state.failed = Some(Failure::new(said.clone(), &failed, tried_with));
                Err(anyhow!(said))
            }
        }
    }

    /// One `rerank` of `passages` for `query`, as they're sent, with `key`:
    /// their scores, in their order, and how many tokens Voyage counted.
    fn request(
        &self,
        key: &str,
        query: &str,
        passages: &[String],
    ) -> Result<(Vec<f32>, u64), Failed> {
        let body = json!({
            "model": self.model,
            "query": query,
            "documents": passages,
            "truncation": true,
        })
        .to_string();
        let url = format!("{}/rerank", self.url);
        let header = format!("Authorization: Bearer {key}");
        let (code, reply) = web_api::post(&url, &[&header], &body, TIMEOUT)?;
        read_reply(code, &reply, passages.len())
    }

    /// How it stands: where the key is, and the last failure since nothing
    /// worked.
    pub fn status(&self) -> Status {
        let state = self.state.lock().unwrap();
        let standing = Standing::of(self.key(), no_key(&self.key_file), state.failed.as_ref());
        Status {
            model: self.model.clone(),
            key: standing.key,
            key_shared: standing.key_shared,
            failed: standing.failed,
            failed_secs_ago: standing.failed_secs_ago,
            tokens: state.tokens,
        }
    }
}

/// What's said when there's no key.
fn no_key(file: &Path) -> String {
    format!(
        "no key for Voyage: put it in {}, or set VOYAGE_API_KEY",
        shell::home_relative(file)
    )
}

/// What `rerank` answers: the scores, the best first, each with the place
/// of what it scored.
#[derive(Deserialize)]
struct Reply {
    #[serde(default)]
    data: Vec<Scored>,
    #[serde(default)]
    usage: Option<Usage>,
}

#[derive(Deserialize)]
struct Scored {
    index: usize,
    relevance_score: f32,
}

#[derive(Deserialize)]
struct Usage {
    #[serde(default)]
    total_tokens: u64,
}

/// What Voyage answers a request it doesn't carry out.
#[derive(Deserialize)]
struct Refusal {
    #[serde(default)]
    detail: String,
}

/// The scores in a reply with HTTP status `code` to a request of `count`
/// passages, in their order, and how many tokens it counted; or why it
/// failed and how long to rest.
fn read_reply(code: u16, reply: &str, count: usize) -> Result<(Vec<f32>, u64), Failed> {
    if code != 200 {
        let detail = serde_json::from_str::<Refusal>(reply)
            .ok()
            .map(|refusal| refusal.detail)
            .filter(|detail| !detail.trim().is_empty());
        let said = match detail {
            Some(detail) => format!("{} (HTTP {code})", detail.trim()),
            None => format!("HTTP {code}"),
        };
        return Err(Failed {
            said,
            rest: web_api::rest_after(code, None),
        });
    }
    let unreadable = |what: String| Failed {
        said: what,
        rest: web_api::REST_REFUSED,
    };
    let reply: Reply = serde_json::from_str(reply)
        .map_err(|err| unreadable(format!("couldn't read its answer: {err}")))?;
    let mut scores = vec![None; count];
    for Scored {
        index,
        relevance_score,
    } in reply.data
    {
        match scores.get_mut(index) {
            Some(score @ None) if relevance_score.is_finite() => *score = Some(relevance_score),
            _ => {
                return Err(unreadable(format!(
                    "it scored passage {index} of {count} twice, or not as a number"
                )));
            }
        }
    }
    let scores: Option<Vec<f32>> = scores.into_iter().collect();
    let scores =
        scores.ok_or_else(|| unreadable(format!("it didn't score all {count} passages")))?;
    let tokens = reply.usage.map_or(0, |usage| usage.total_tokens);
    Ok((scores, tokens))
}

/// What `tokens` cost, as `rerank-3`'s price list has it, past its free
/// tokens.
pub fn cost(tokens: u64) -> String {
    format!(
        "about ${:.4} at rerank-3's price",
        tokens as f64 * USD_PER_MILLION_TOKENS / 1e6
    )
}

/// The API, made once in each process for the settings as they are and
/// kept, with what it scored and how it last failed; made again when they
/// change.
pub fn shared(settings: &MemorySettings) -> Arc<Voyage> {
    static SHARED: Mutex<Option<Arc<Voyage>>> = Mutex::new(None);
    let wanted = Voyage::new(settings);
    let mut shared = SHARED.lock().unwrap();
    if let Some(voyage) = &*shared
        && voyage.model == wanted.model
        && voyage.key_file == wanted.key_file
        && voyage.url == wanted.url
    {
        return voyage.clone();
    }
    let voyage = Arc::new(wanted);
    *shared = Some(voyage.clone());
    voyage
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::web_api::fake::{self, Fake};

    fn voyage_at(key_file: PathBuf, url: &str) -> Voyage {
        Voyage::for_tests(key_file, url)
    }

    /// Scores each passage by its place, the last best, the best first as
    /// Voyage gives them.
    fn by_place(body: &str) -> (u16, String) {
        let body: serde_json::Value = serde_json::from_str(body).unwrap();
        let count = body["documents"].as_array().unwrap().len();
        let data: Vec<String> = (0..count)
            .rev()
            .map(|at| format!("{{\"relevance_score\": 0.{at}5, \"index\": {at}}}"))
            .collect();
        let reply = format!(
            "{{\"object\": \"list\", \"data\": [{}], \"model\": \"rerank-2.5\", \
             \"usage\": {{\"total_tokens\": {}}}}}",
            data.join(","),
            count * 10
        );
        (200, reply)
    }

    #[test]
    fn the_key_is_read_from_its_file_then_voyage_s_variable() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("voyage.key");
        let from_env = |name: &str| (name == "VOYAGE_API_KEY").then(|| "pa-fake-2".to_string());
        assert_eq!(
            web_api::key_from(&file, &KEY_VARIABLES, from_env).unwrap(),
            Some(("pa-fake-2".into(), KeyFrom::Variable("VOYAGE_API_KEY")))
        );
        std::fs::write(&file, "pa-fake_key-1\n").unwrap();
        assert_eq!(
            web_api::key_from(&file, &KEY_VARIABLES, from_env).unwrap(),
            Some(("pa-fake_key-1".into(), KeyFrom::File(file.clone())))
        );
    }

    #[test]
    fn a_reply_gives_the_scores_in_the_passages_order_or_says_why_it_failed() {
        let reply = r#"{"object": "list", "data": [
            {"relevance_score": 0.9, "index": 1}, {"relevance_score": 0.25, "index": 0}],
            "model": "rerank-2.5", "usage": {"total_tokens": 30}}"#;
        assert_eq!(read_reply(200, reply, 2).unwrap(), (vec![0.25, 0.9], 30));
        assert!(read_reply(200, reply, 3).is_err(), "one not scored");
        let twice = r#"{"data": [{"relevance_score": 0.9, "index": 0},
            {"relevance_score": 0.2, "index": 0}]}"#;
        assert!(read_reply(200, twice, 2).is_err());
        let beyond = r#"{"data": [{"relevance_score": 0.9, "index": 5}]}"#;
        assert!(read_reply(200, beyond, 1).is_err());
        let refused = r#"{"detail": "Provided API key is invalid."}"#;
        let failed = read_reply(401, refused, 1).unwrap_err();
        assert_eq!(failed.said, "Provided API key is invalid. (HTTP 401)");
        assert_eq!(failed.rest, web_api::REST_REFUSED);
        let quota = read_reply(429, "", 1).unwrap_err();
        assert_eq!(quota.said, "HTTP 429");
        assert_eq!(quota.rest, web_api::REST_QUOTA);
        assert_eq!(
            read_reply(502, "<html>", 1).unwrap_err().rest,
            web_api::REST_OFFLINE
        );
    }

    #[test]
    fn requests_carry_the_key_in_a_header_and_a_search_is_scored_once() {
        let fake = Fake::new("/v1", by_place);
        let dir = tempfile::tempdir().unwrap();
        let key_file = dir.path().join("voyage.key");
        std::fs::write(&key_file, "pa-fake-for-tests\n").unwrap();
        let voyage = voyage_at(key_file, &fake.url);
        let passages = [
            "Postgres has to be up",
            "export TOKEN=sk-fake0123456789abcdefghij",
        ];
        assert_eq!(
            voyage.rerank("start the db", &passages).unwrap(),
            [0.05, 0.15]
        );
        assert_eq!(
            voyage.rerank("start the db", &passages).unwrap(),
            [0.05, 0.15]
        );
        assert_eq!(
            voyage.rerank("start the db", &passages[..1]).unwrap(),
            [0.05]
        );
        assert!(voyage.rerank("anything", &[]).unwrap().is_empty());
        let asked = fake.asked();
        assert_eq!(asked.len(), 2, "the same asked again isn't sent again");
        let (head, body) = &asked[0];
        assert!(head.starts_with("POST /v1/rerank "), "{head}");
        assert!(
            head.contains("Authorization: Bearer pa-fake-for-tests\r\n"),
            "{head}"
        );
        assert!(!head.lines().next().unwrap().contains("pa-fake"), "{head}");
        let body: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(body["model"], "rerank-3");
        assert_eq!(body["query"], "start the db");
        assert_eq!(
            body["documents"],
            json!(["Postgres has to be up", "export TOKEN=[redacted]"])
        );
        let status = voyage.status();
        assert_eq!(status.model, "rerank-3");
        assert_eq!(status.tokens, 30);
        assert_eq!(status.failed, None);
        assert!(status.key.unwrap().ends_with("voyage.key"));
    }

    #[test]
    fn a_failure_is_kept_and_nothing_asked_until_the_rest_is_over_or_the_key_changes() {
        let fake = Fake::new("/v1", |_| {
            (
                401,
                r#"{"detail": "Provided API key pa-fake-1 is invalid."}"#.to_string(),
            )
        });
        let dir = tempfile::tempdir().unwrap();
        let key_file = dir.path().join("voyage.key");
        std::fs::write(&key_file, "pa-fake-1").unwrap();
        let voyage = voyage_at(key_file.clone(), &fake.url);
        let err = format!("{:#}", voyage.rerank("one", &["a"]).unwrap_err());
        assert!(err.starts_with("rerank-3: Provided API key"), "{err}");
        assert!(!err.contains("pa-fake-1"), "{err}");
        assert!(voyage.rerank("two", &["b"]).is_err());
        assert_eq!(fake.asked().len(), 1, "resting after a refusal");
        let status = voyage.status();
        assert!(status.failed.unwrap().contains("is invalid"));
        assert!(status.failed_secs_ago.is_some());
        std::fs::write(&key_file, "pa-fake-2").unwrap();
        assert!(voyage.rerank("three", &["c"]).is_err());
        assert_eq!(fake.asked().len(), 2, "a new key is tried at once");
        let offline = voyage_at(key_file, &fake::nowhere());
        assert!(offline.rerank("four", &["d"]).is_err());
        let unkeyed = voyage_at(dir.path().join("missing.key"), &fake.url);
        let err = format!("{:#}", unkeyed.rerank("five", &["e"]).unwrap_err());
        assert!(err.starts_with("no key for Voyage"), "{err}");
        assert_eq!(unkeyed.status().key, None);
        assert_eq!(fake.asked().len(), 2);
    }

    #[test]
    fn every_model_cuts_between_what_it_scored_on_crystal_s_memory() {
        // The best a search nothing answered scored, the least the best for
        // one something answered did, and the most two different lessons
        // on one subject did, each model's.
        for (model, nothing, answered, different) in [
            ("rerank-3", 0.309, 0.543, 0.867),
            ("rerank-3-lite", 0.365, 0.590, 0.875),
            ("rerank-2.5", 0.387, 0.594, 0.867),
        ] {
            let measured = thresholds(model);
            assert!(nothing < measured.answers_from, "{model}");
            assert!(measured.answers_from < answered, "{model}");
            assert!(measured.kept_from <= measured.answers_from, "{model}");
            assert!(different < measured.same_reranked_from, "{model}");
            assert!(measured.same_reranked_from < 1.0, "{model}");
        }
        assert_eq!(MODELS.len(), 3);
        assert_eq!(thresholds(MODEL), thresholds("rerank-3"));
    }
}
