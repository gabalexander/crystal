//! Asking a web API with `curl`, as memory asks Google's Gemini API for
//! vectors ([`crate::gemini`]) and Voyage AI's to rerank
//! ([`crate::voyage`]): the key read for each request, from its file or
//! else the environment, so a new one counts at once, and given to `curl`
//! on its standard input with the request, never on its command line,
//! where `ps` shows it; what's said of a failure scrubbed of it; and how
//! long to rest after one, so that a search meanwhile goes on at once
//! without the API rather than waiting on it.
//!
//! `curl`, as the models' downloads and `crystal update` use, rather than
//! an HTTP client and a TLS stack in crystal's static builds.

use crate::{printable, secrets, shell};
use anyhow::{Result, anyhow};
use std::fmt;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

/// How long no request is tried after one failed: after the network or the
/// API failing, after a 429 that doesn't say, and the most one that does
/// may ask; and after the API refused the request or the key, unless the
/// key changes.
pub const REST_OFFLINE: Duration = Duration::from_secs(30);
pub const REST_QUOTA: Duration = Duration::from_secs(60);
pub const REST_MOST: Duration = Duration::from_secs(600);
pub const REST_REFUSED: Duration = Duration::from_secs(300);

/// How long `curl` may take to connect, in seconds.
const CONNECT_TIMEOUT: u64 = 5;

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
/// of `variables` that `variable` gives, `None` with neither. One with a
/// character no key has is refused, without saying it.
pub fn key_from(
    file: &Path,
    variables: &[&'static str],
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
        variables.iter().find_map(|name| {
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
pub fn shared_file(file: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(file).is_ok_and(|meta| meta.permissions().mode() & 0o077 != 0)
}

/// A hash of the key, to tell it from another without keeping it.
pub fn hash(key: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    hasher.finish()
}

/// `said` made fit to show and keep: one line, not too long, and nothing in
/// it that could be the key or another credential.
pub fn scrubbed(said: &str, key: &str) -> String {
    let said = said.replace(key, secrets::REDACTED);
    let said = secrets::redact(&printable::line(&said));
    let mut said: String = said.split_whitespace().collect::<Vec<_>>().join(" ");
    if said.chars().count() > 240 {
        said = said.chars().take(239).collect::<String>() + "…";
    }
    said
}

/// `text` without credentials, cut to `most` bytes at most.
pub fn sent(text: &str, most: usize) -> String {
    let mut text = secrets::redact(text);
    let mut end = text.len().min(most);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    text
}

/// Why a request failed, and how long to wait before another.
#[derive(Debug, PartialEq)]
pub struct Failed {
    pub said: String,
    pub rest: Duration,
}

/// A request that failed: what's said of it, when, how long until another
/// is tried, and a hash of the key it was tried with, so that a new key is
/// tried at once.
pub struct Failure {
    pub said: String,
    pub at: SystemTime,
    pub until: Instant,
    pub key: u64,
}

impl Failure {
    /// What `failed` with the key whose hash is `key` is kept as, `said`
    /// with what it's about ahead of it.
    pub fn new(said: String, failed: &Failed, key: u64) -> Failure {
        Failure {
            said,
            at: SystemTime::now(),
            until: Instant::now() + failed.rest,
            key,
        }
    }

    /// Whether no request is to be tried with the key whose hash is `key`.
    pub fn resting(&self, key: u64) -> bool {
        self.key == key && Instant::now() < self.until
    }

    pub fn secs_ago(&self) -> Option<u64> {
        Some(SystemTime::now().duration_since(self.at).ok()?.as_secs())
    }
}

/// How an API stands, as its status says it: where its key was found, or
/// why there's none; whether others can read its file; and why it last
/// failed and how many seconds ago, while nothing has worked since.
pub struct Standing {
    pub key: Option<String>,
    pub key_shared: bool,
    pub failed: Option<String>,
    pub failed_secs_ago: Option<u64>,
}

impl Standing {
    /// How it stands with `key` as it was looked for, `no_key` said with
    /// none, and `last` the request that failed last.
    pub fn of(
        key: Result<Option<(String, KeyFrom)>>,
        no_key: String,
        last: Option<&Failure>,
    ) -> Standing {
        let (key, failed) = match key {
            Ok(Some((_, from))) => (Some(from), None),
            Ok(None) => (None, Some(no_key)),
            Err(err) => (None, Some(format!("{err:#}"))),
        };
        Standing {
            key_shared: matches!(&key, Some(KeyFrom::File(file)) if shared_file(file)),
            key: key.map(|from| from.to_string()),
            failed: failed.or_else(|| last.map(|failure| failure.said.clone())),
            failed_secs_ago: last.and_then(Failure::secs_ago),
        }
    }
}

/// What `curl` reads as its config: `headers` (the key's among them), and
/// the request. What's in quotes takes `\` before `"` and `\`, and writes a
/// line's end and a tab as `\n`, `\r` and `\t`.
fn curl_config(headers: &[&str], body: &str) -> String {
    let quote = |text: &str| {
        let mut quoted = String::with_capacity(text.len() + 16);
        for c in text.chars() {
            match c {
                '\\' => quoted.push_str("\\\\"),
                '"' => quoted.push_str("\\\""),
                '\n' => quoted.push_str("\\n"),
                '\r' => quoted.push_str("\\r"),
                '\t' => quoted.push_str("\\t"),
                c => quoted.push(c),
            }
        }
        quoted
    };
    let mut config = String::with_capacity(body.len() + 128);
    for header in headers {
        config.push_str(&format!("header = \"{}\"\n", quote(header)));
    }
    config.push_str("header = \"Content-Type: application/json\"\n");
    config.push_str(&format!("data-binary = \"{}\"\n", quote(body)));
    config
}

/// POSTs `body` to `url` with `curl`, `headers` (the key's among them) and
/// the body given on its standard input, within `timeout` seconds: the
/// HTTP status and what came back.
pub fn post(
    url: &str,
    headers: &[&str],
    body: &str,
    timeout: u64,
) -> Result<(u16, String), Failed> {
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
    let config = curl_config(headers, body);
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

/// How long to rest after a reply with HTTP status `code` that isn't a
/// 200: as long as `asked` for a 429, within reason, or else a minute; 30
/// seconds after the API failing; five minutes after it refused.
pub fn rest_after(code: u16, asked: Option<Duration>) -> Duration {
    match code {
        429 => asked
            .unwrap_or(REST_QUOTA)
            .clamp(Duration::from_secs(5), REST_MOST),
        500.. => REST_OFFLINE,
        _ => REST_REFUSED,
    }
}

/// A web server of a test's own on 127.0.0.1, which answers each request
/// as `answer` says, given its body, and keeps each request's head and body.
#[cfg(test)]
pub mod fake {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    pub struct Fake {
        /// Where it listens, with `path` after it.
        pub url: String,
        asked: Arc<Mutex<Vec<(String, String)>>>,
    }

    impl Fake {
        pub fn new(path: &str, answer: fn(&str) -> (u16, String)) -> Fake {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}{path}", listener.local_addr().unwrap());
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
                    let (code, reply) = answer(&body);
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

        pub fn asked(&self) -> Vec<(String, String)> {
            self.asked.lock().unwrap().clone()
        }
    }

    /// A URL nothing listens on: a port bound and let go, as offline.
    pub fn nowhere() -> String {
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        format!("http://{port}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_key_is_read_from_its_file_then_the_environment_and_checked() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("gemini.key");
        let variables = ["GEMINI_API_KEY", "GOOGLE_API_KEY"];
        let none = |_: &str| None;
        assert_eq!(key_from(&file, &variables, none).unwrap(), None);
        let from_google = |name: &str| (name == "GOOGLE_API_KEY").then(|| "AQ.fake-2".to_string());
        assert_eq!(
            key_from(&file, &variables, from_google).unwrap(),
            Some(("AQ.fake-2".into(), KeyFrom::Variable("GOOGLE_API_KEY")))
        );
        let both = |name: &str| Some(format!("{name}-fake"));
        assert_eq!(
            key_from(&file, &variables, both).unwrap().unwrap().1,
            KeyFrom::Variable("GEMINI_API_KEY")
        );
        std::fs::write(&file, "  AQ.fake_key-1\nignored\n").unwrap();
        assert_eq!(
            key_from(&file, &variables, both).unwrap(),
            Some(("AQ.fake_key-1".into(), KeyFrom::File(file.clone())))
        );
        // An empty file leaves it to the environment.
        std::fs::write(&file, "\n").unwrap();
        assert_eq!(
            key_from(&file, &variables, both).unwrap().unwrap().1,
            KeyFrom::Variable("GEMINI_API_KEY")
        );
        // A key that would break out of its header is refused, unsaid.
        std::fs::write(&file, "fake\" secret-x").unwrap();
        let err = format!("{:#}", key_from(&file, &variables, none).unwrap_err());
        assert!(err.contains("characters no key has"), "{err}");
        assert!(!err.contains("secret"), "{err}");
        assert_eq!(
            KeyFrom::Variable("GEMINI_API_KEY").to_string(),
            "$GEMINI_API_KEY"
        );
    }

    #[test]
    fn curl_reads_the_key_in_a_header_and_the_request_quoted() {
        let body = json!({"text": "say \"hi\" \\ then\nstop\t."}).to_string();
        let config = curl_config(&["x-goog-api-key: AQ.fake"], &body);
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
    fn what_is_sent_has_no_credentials_and_is_cut_whole_characters() {
        assert_eq!(
            sent("export OPENAI_API_KEY=sk-fake0123456789abcdefghij", 100),
            "export OPENAI_API_KEY=[redacted]"
        );
        let cut = sent(&"é".repeat(10), 5);
        assert_eq!(cut, "éé");
    }

    #[test]
    fn what_is_said_of_a_failure_never_has_the_key() {
        let said = scrubbed(
            "bad key AQ.fake-key-123\u{7} in\nthe   header",
            "AQ.fake-key-123",
        );
        assert_eq!(said, "bad key [redacted] in the header");
    }

    #[test]
    fn a_failure_rests_as_long_as_a_429_asks_within_reason() {
        assert_eq!(rest_after(429, None), REST_QUOTA);
        assert_eq!(
            rest_after(429, Some(Duration::from_secs(37))),
            Duration::from_secs(37)
        );
        assert_eq!(rest_after(429, Some(Duration::from_secs(1))).as_secs(), 5);
        assert_eq!(rest_after(429, Some(Duration::from_secs(9999))), REST_MOST);
        assert_eq!(rest_after(503, None), REST_OFFLINE);
        assert_eq!(rest_after(401, None), REST_REFUSED);
    }
}
