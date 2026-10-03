//! Taking credentials out of text before crystal keeps it: what a model or
//! a transcript carries into a project's memory is shown to every later
//! session there, and promoted, written into the repo's own docs. It errs
//! toward taking out too much: a lost word in a note costs less than a
//! leaked key.

/// What a credential is replaced with.
pub const REDACTED: &str = "[redacted]";

/// `text` with what looks like a credential replaced by [`REDACTED`]: a
/// PEM private key, a URL's password, the value given to a secret's name
/// (`API_KEY=…`, `"password": "…"`), what follows `Bearer` or
/// `Authorization:`, and any word in a known provider's key format, or long
/// and random enough to be one.
pub fn redact(text: &str) -> String {
    let text = redact_url_passwords(&redact_private_keys(text));
    let mut out = String::with_capacity(text.len());
    let mut expect = Expect::Nothing;
    let mut rest = text.as_str();
    loop {
        let start = rest.find(is_token_char).unwrap_or(rest.len());
        let gap = &rest[..start];
        out.push_str(gap);
        if start == rest.len() {
            break;
        }
        let tail = &rest[start..];
        let len = tail.find(|c: char| !is_token_char(c)).unwrap_or(tail.len());
        let raw = &tail[..len];
        rest = &tail[len..];
        // A sentence's full stop isn't part of the word.
        let word = raw.trim_end_matches('.');
        let (secret, next) = judge(word, gap, expect);
        expect = next;
        if secret {
            out.push_str(REDACTED);
            out.push_str(&raw[word.len()..]);
        } else {
            out.push_str(raw);
        }
    }
    out
}

/// What the word before this one said about it.
#[derive(Clone, Copy, PartialEq)]
enum Expect {
    Nothing,
    /// A secret's name (`api_key`, `GITHUB_TOKEN`, `password`): a value
    /// given to it with `=` or `:` is the secret.
    Value,
    /// `Bearer`, or the scheme after `Authorization:`: the next word is.
    Credential,
}

/// What a credential is made of, and the words around it are split on:
/// letters, digits and `_ - + / . ~`.
fn is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '+' | '/' | '.' | '~')
}

/// Whether `word` (after `gap`, following a word that said `expect`) is a
/// secret, and what it says about the word after it.
fn judge(word: &str, gap: &str, expect: Expect) -> (bool, Expect) {
    let lower = word.to_ascii_lowercase();
    match expect {
        Expect::Value => {
            let sign: String = gap
                .chars()
                .filter(|c| !c.is_whitespace() && !matches!(c, '"' | '\''))
                .collect();
            if sign == "=" || sign == ":" {
                if matches!(lower.as_str(), "bearer" | "basic" | "token" | "bot") {
                    return (false, Expect::Credential);
                }
                if sign == "=" || looks_random(word) {
                    return (true, Expect::Nothing);
                }
            }
        }
        Expect::Credential => {
            if !gap.is_empty() && gap.chars().all(|c| c == ' ' || c == '\t') && looks_random(word) {
                return (true, Expect::Nothing);
            }
        }
        Expect::Nothing => {}
    }
    if looks_like_a_key(word) {
        return (true, Expect::Nothing);
    }
    if lower == "bearer" {
        return (false, Expect::Credential);
    }
    if names_a_secret(&lower) {
        return (false, Expect::Value);
    }
    (false, Expect::Nothing)
}

/// A word that names a secret: `OPENAI_API_KEY`, `client_secret`,
/// `password`, `GITHUB_TOKEN`, `Authorization`.
fn names_a_secret(lower: &str) -> bool {
    [
        "api_key",
        "apikey",
        "api-key",
        "secret",
        "password",
        "passwd",
        "access_key",
        "private_key",
        "credential",
    ]
    .iter()
    .any(|name| lower.contains(name))
        || ["token", "authorization", "pwd"]
            .iter()
            .any(|name| lower.ends_with(name))
}

/// A word shaped like a credential whatever is around it: a known
/// provider's key format, a JWT, or a long run of letters and digits in
/// both cases (not a path, not a hex hash).
fn looks_like_a_key(word: &str) -> bool {
    let len = word.len();
    let has = |f: fn(char) -> bool| word.chars().any(f);
    let starts = |prefixes: &[&str]| prefixes.iter().any(|prefix| word.starts_with(prefix));
    let rest_upper_alnum = |from: usize| {
        word[from..]
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
    };
    (word.starts_with("sk-") && len >= 20)
        || (starts(&["sk_live_", "sk_test_", "rk_live_", "rk_test_"]) && len >= 16)
        || (starts(&[
            "ghp_",
            "gho_",
            "ghu_",
            "ghs_",
            "ghr_",
            "github_pat_",
            "glpat-",
        ]) && len >= 20)
        || (starts(&["xoxb-", "xoxp-", "xoxa-", "xoxr-", "xoxs-"]) && len >= 15)
        || (starts(&["AKIA", "ASIA"]) && len == 20 && rest_upper_alnum(4))
        || (word.starts_with("AIza") && len >= 35)
        || (word.starts_with("ya29.") && len >= 20)
        || (word.starts_with("npm_") && len >= 36)
        || (word.starts_with("hf_") && len >= 30)
        || (word.starts_with("eyJ") && len >= 30 && word.matches('.').count() == 2)
        || (len >= 32
            && !word.contains('/')
            && !word.contains('.')
            && has(|c| c.is_ascii_uppercase())
            && has(|c| c.is_ascii_lowercase())
            && has(|c| c.is_ascii_digit()))
}

/// A value after `name:` that's a credential rather than prose: long, or
/// not a plain word.
fn looks_random(word: &str) -> bool {
    word.len() >= 16 || (word.len() >= 8 && word.chars().any(|c| c.is_ascii_digit()))
}

/// PEM private keys, header to footer, as one [`REDACTED`].
fn redact_private_keys(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("-----BEGIN ") {
        let header_end = rest[at..].find('\n').map_or(rest.len(), |n| at + n);
        if !rest[at..header_end].contains("PRIVATE KEY") {
            out.push_str(&rest[..at + 5]);
            rest = &rest[at + 5..];
            continue;
        }
        out.push_str(&rest[..at]);
        out.push_str(REDACTED);
        rest = match rest[at..].find("-----END ") {
            Some(end) => {
                let from = at + end + "-----END ".len();
                match rest[from..].find("-----") {
                    Some(close) => &rest[from + close + 5..],
                    None => "",
                }
            }
            None => "",
        };
    }
    out.push_str(rest);
    out
}

/// `scheme://user:password@host` with the password as [`REDACTED`].
fn redact_url_passwords(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("://") {
        let start = at + 3;
        let authority_end = rest[start..]
            .find(|c: char| c == '/' || c.is_whitespace() || matches!(c, '"' | '\'' | '>' | '`'))
            .map_or(rest.len(), |n| start + n);
        let authority = &rest[start..authority_end];
        match (authority.rfind('@'), authority.find(':')) {
            (Some(amp), Some(colon)) if colon < amp => {
                out.push_str(&rest[..start + colon + 1]);
                out.push_str(REDACTED);
                out.push_str(&rest[start + amp..authority_end]);
            }
            _ => out.push_str(&rest[..authority_end]),
        }
        rest = &rest[authority_end..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_are_taken_out_and_the_prose_around_them_kept() {
        let cases = [
            (
                "export ANTHROPIC_API_KEY=sk-ant-api03-AbCdEf0123456789xyz",
                "export ANTHROPIC_API_KEY=[redacted]",
            ),
            (
                "curl -H 'Authorization: Bearer abcDEF1234567890ghijk' https://x",
                "curl -H 'Authorization: Bearer [redacted]' https://x",
            ),
            (
                "Authorization: Basic dXNlcjpwYXNzd29yZA==",
                "Authorization: Basic [redacted]==",
            ),
            (
                "the token is ghp_0123456789abcdefghijABCDEFGHIJ012345.",
                "the token is [redacted].",
            ),
            (
                "{\"password\": \"hunter22x\"}",
                "{\"password\": \"[redacted]\"}",
            ),
            ("DB_PASSWORD=pw", "DB_PASSWORD=[redacted]"),
            (
                "clone https://bot:s3cr3t@github.com/o/r.git",
                "clone https://bot:[redacted]@github.com/o/r.git",
            ),
            ("aws AKIAIOSFODNN7EXAMPLE here", "aws [redacted] here"),
            (
                "slack xoxb-123456789012-abcdefghij works",
                "slack [redacted] works",
            ),
            (
                "jwt eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.c2lnbmF0dXJlX2hlcmU",
                "jwt [redacted]",
            ),
            (
                "a\n-----BEGIN RSA PRIVATE KEY-----\nMIIE\nabcd\n-----END RSA PRIVATE KEY-----\nb",
                "a\n[redacted]\nb",
            ),
        ];
        for (raw, want) in cases {
            assert_eq!(redact(raw), want, "{raw}");
        }
    }

    #[test]
    fn prose_that_only_talks_of_secrets_stays_as_it_was() {
        for prose in [
            "Use `make ci` before pushing; the token refresh runs hourly.",
            "Bearer tokens expire after an hour",
            "src/memory.rs and d0ba4e321a7e481237e0360dbd3a3f9518131c94",
            "see https://github.com/o/r/pull/12",
            "-----BEGIN CERTIFICATE----- is public",
        ] {
            assert_eq!(redact(prose), prose);
        }
    }
}
