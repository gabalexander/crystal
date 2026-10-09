//! What the wiki's writers are told, and what they answer: one writer a
//! subsection, given the outline and its files numbered; one a section's
//! summary, given its subsections; one the overview, given the sections;
//! and the writer told what the checks found, asked once to put it right.
//!
//! The instructions decide the page: Code Wiki's is concrete, names every
//! function, type and file it talks about, links each to its lines, and
//! explains how the parts work together rather than listing them. The
//! writer is shown an example of a subsection in that manner.

use super::model::Diagram;
use serde_json::{Value, json};

/// What every writer is told about the page, the links and the diagram.
const RULES: &str = "\
The reader is an engineer new to this codebase who wants to understand how it works, well \
enough to find their way in the code and change it.\n\n\
How to write:\n\
- Be concrete. Name the real functions, methods, types, fields, constants, files, flags, \
config keys and commands, each in backticks. A sentence that could be said of any project \
(\"handles errors robustly\", \"provides a flexible interface\") says nothing: say which error, \
which interface, what is done with it.\n\
- Be accurate. Say only what you read in the code, its comments and the commits shown. If you \
haven't read it, Read it or leave it out. Never guess a name, a path, a line or what something \
does.\n\
- Explain how things work and work together: what calls what, in what order, what data goes \
where, what decision is made where and why (when the code, a comment or a commit says why).\n\
- No headings and no title (the page has them), no closing summary, no \"In summary\" or \"In \
conclusion\", no words like robust, seamless, powerful, comprehensive, leverage, crucial, \
streamline, facilitate. Plain, exact English.\n\n\
Links:\n\
- Link each function, type, constant, field, flag, config key and file where you first name it, \
and again where it matters: [`Name`](code:PATH#L10-L42), PATH from the top of the repository, \
the lines its definition takes, from its first line to its last, as the numbered listing or \
Read shows them. A file is [`src/a.rs`](code:src/a.rs), a directory [`src/tui/`](code:src/tui/), \
one line #L10. Never link to lines you haven't seen: a wrong link is worse than none.\n\
- When you mention something another part of the page covers, link it by its id from the \
outline, [Its Title](#its-id), and leave it to that part. Never make up an id.\n\n\
The diagram (mermaid):\n\
- A flowchart (TD or LR) for structure, or for how a call or data flows; a sequenceDiagram \
when the point is an exchange between a few parties over time; a classDiagram or erDiagram when \
types or tables and how they relate are the point.\n\
- Boxes are the real components, labelled with their name and, on a second line, the file or \
directory they're in: worker[\"Worker<br/>(internal/queue/worker.go)\"]. Arrows say in a few \
words what goes along them: a -->|claims due jobs| b. A dashed arrow (-.->) for what happens on \
failure or seldom.\n\
- 4 to 12 boxes. Node ids are letters, digits and underscores, never a mermaid keyword (end, \
graph, subgraph, style, class). Every node label in double quotes. No colours, styles, classDef, \
click or subgraph styling; no HTML but <br/>; no semicolons or double quotes inside labels or \
messages.\n\
- Its caption is one sentence on what it shows.";

/// An example of a subsection in the manner wanted, from a made-up
/// repository so that nothing of it is copied into a real one.
const EXAMPLE: &str = r#"An example of a good subsection, from another repository, a job queue written in Go. Its body_md:

Retries live in [`internal/queue/retry.go`](code:internal/queue/retry.go), and every failed job goes through [`Retrier.Handle`](code:internal/queue/retry.go#L48-L96) before anything else sees it. A worker that gets an error back from a job's [`Run`](code:internal/queue/job.go#L22) doesn't decide what happens next: it hands the job and the error to the retrier, which either schedules another attempt or moves the job to the dead-letter table described in [Dead Letters and Replays](#dead-letters-and-replays).

The decision rests on two things the job carries, its [`Attempts`](code:internal/queue/job.go#L15) count and the [`RetryPolicy`](code:internal/queue/policy.go#L9-L31) its type registered with:

- **Retryable errors.** [`IsRetryable`](code:internal/queue/errors.go#L40-L58) treats timeouts, `ECONNRESET` and HTTP 5xx answers as transient; a validation error, or anything wrapped in [`Permanent`](code:internal/queue/errors.go#L12), goes straight to the dead letters, however many attempts are left.
- **Backoff.** [`RetryPolicy.Next`](code:internal/queue/policy.go#L35-L52) doubles the delay from `InitialDelay` up to `MaxDelay`, then adds up to 20% jitter, so that a burst of failures doesn't come back as a burst.
- **The cap.** Once `Attempts` reaches `MaxAttempts` (5 unless the job type says otherwise), the job is dead whatever the error.

Scheduling another attempt doesn't hold a worker. [`Retrier.Handle`](code:internal/queue/retry.go#L48-L96) writes the job back with `run_at` set in the future, in the same transaction that records the error in `job_errors`, so a crash between the two can neither lose the job nor run it twice. The poller only claims jobs whose `run_at` has passed ([`Poller.claim`](code:internal/queue/poller.go#L61-L88)), which is all it takes for the delay to hold, and because the attempt count goes up in that same `UPDATE`, two workers that both time out on one job can't both count it.

The retrier is the one place errors are classified, so the counters in [Queue Metrics](#queue-metrics), `queue_retries_total` and `queue_dead_total`, are incremented there, labelled by job type and by the reason [`IsRetryable`](code:internal/queue/errors.go#L40-L58) gave.

Its diagram:

flowchart TD
  worker["Worker<br/>(internal/queue/worker.go)"] -->|job failed| retrier["Retrier.Handle<br/>(internal/queue/retry.go)"]
  retrier -->|retryable, attempts left| policy["RetryPolicy.Next<br/>(internal/queue/policy.go)"]
  policy -->|run_at = now + backoff| jobs[("jobs table")]
  retrier -.->|permanent, or out of attempts| dead["Dead letters<br/>(internal/queue/dead.go)"]
  poller["Poller.claim<br/>(internal/queue/poller.go)"] -->|claims jobs whose run_at passed| jobs

Its caption: A failed job goes back to the jobs table with a later run_at, or to the dead letters."#;

/// What a subsection's writer is told.
pub fn subsection_system() -> String {
    format!(
        "You are writing one subsection of a wiki about a software repository, in the manner of \
         Google's Code Wiki: one long page, section by section, each subsection a diagram and \
         prose that explains one part of the code. You have Read, Grep and Glob over the \
         repository at the commit the wiki is written from; you can't run anything.\n\n\
         The message gives you the whole outline (so you can link the other parts, and leave \
         them to their writers), what your subsection is to explain, the commits that touched \
         its files lately, the definitions in its files with their lines, and its files \
         themselves, numbered. Read more where you need it: callers in other files, the types \
         it uses.\n\n\
         Write body_md: 400 to 900 words in four to eight paragraphs, with a bullet list where \
         you go through several things of a kind (each bullet starting with the thing, linked, \
         then what it does). Open with what this part is for and where it lives, then how it \
         works.\n\n{RULES}\n\n{EXAMPLE}\n\n\
         Answer with the structured output: body_md, and diagram with its mermaid and caption."
    )
}

/// What a section's summary writer is told.
pub fn section_system() -> String {
    format!(
        "You are writing the introduction of one top section of a wiki about a software \
         repository, in the manner of Google's Code Wiki. The message gives you the outline, \
         and the section's subsections as their writers wrote them. You have Read, Grep and \
         Glob over the repository, to check what you say.\n\n\
         Write summary_md: 150 to 400 words in two to four paragraphs on what this part of the \
         system does as a whole, its main pieces and how they work together, each subsection \
         linked by its id where its subject comes up, [Its Title](#its-id), so the reader knows \
         where to go next, and the main entry points linked into the code. Explain; don't list \
         the subsections one after another. The diagram shows how the section's parts fit \
         together: a box for each main component (often one a subsection), arrows for the calls, \
         data or control between them.\n\n{RULES}\n\n\
         Answer with the structured output: summary_md, and diagram with its mermaid and caption."
    )
}

/// What the overview's writer is told.
pub fn overview_system() -> String {
    format!(
        "You are writing the overview at the top of a wiki about a software repository, in the \
         manner of Google's Code Wiki. The message gives you the outline and every section's \
         summary. You have Read, Grep and Glob over the repository, to check what you say.\n\n\
         Write summary_md: 300 to 600 words in three to six paragraphs: what the repository is \
         and what it's for; its architecture, the main parts and how a typical request, run or \
         session goes through them; how it's built and run. Link every section by its id where \
         its subject comes up, so that the overview sends the reader to each, as in \"These are \
         detailed in [Its Title](#its-id).\", and link the main entry points into the code. The \
         diagram is the architecture: the main components, 6 to 15 boxes, each labelled with \
         its name and its directory or file, and how they connect.\n\n{RULES}\n\n\
         Answer with the structured output: summary_md, and diagram with its mermaid and caption."
    )
}

/// What a writer asked to put its text right is told, besides what it
/// was told before.
pub fn fix_system(first: &str) -> String {
    format!(
        "{first}\n\nThis time you are given a text you wrote, and the problems the checks found \
         in it. Put right each problem and change nothing else: a link at the wrong lines goes \
         where the thing is defined (find it with Grep and Read), a path the repository doesn't \
         have is replaced by the right one or left unlinked, an id the page doesn't have by one \
         from the outline or no link, a diagram that doesn't read is written again in the subset \
         above. Answer with the whole text and the diagram, as before."
    )
}

/// What an update adds: the text before, and what changed.
pub const UPDATE: &str = "This is an update. The code changed since the text below was \
written: the diff of its files is below it. Keep what still holds, word for word where you \
can, and change what the diff changed; check links whose lines moved. Set meaning_changed to \
true only when what this part does, its components or how they connect changed, so that what \
sums it up elsewhere must change too; not for wording, a private helper renamed or lines moved.";

/// The shape of a subsection's answer, `body_md` or with `summary`,
/// `summary_md`; with `update`, saying whether its meaning changed.
pub fn schema(summary: bool, update: bool) -> Value {
    let text_key = if summary { "summary_md" } else { "body_md" };
    let mut properties = serde_json::Map::new();
    properties.insert(text_key.into(), json!({"type": "string"}));
    properties.insert(
        "diagram".into(),
        json!({
            "type": "object",
            "properties": {
                "mermaid": {"type": "string"},
                "caption": {"type": "string"},
            },
            "required": ["mermaid", "caption"],
        }),
    );
    let mut required = vec![json!(text_key), json!("diagram")];
    if update {
        properties.insert("meaning_changed".into(), json!({"type": "boolean"}));
        required.push(json!("meaning_changed"));
    }
    json!({"type": "object", "properties": properties, "required": required})
}

/// What a writer answered.
#[derive(Debug, Clone, PartialEq)]
pub struct Written {
    pub text: String,
    pub diagram: Option<Diagram>,
    /// For an update, whether what it says changed in meaning.
    pub meaning_changed: bool,
}

/// The answer read: its text, at `body_md` or `summary_md`, its diagram,
/// when it has one, and whether its meaning changed.
pub fn read(answer: &Value) -> Option<Written> {
    let text = answer["body_md"]
        .as_str()
        .or_else(|| answer["summary_md"].as_str())?
        .trim()
        .to_string();
    if text.is_empty() {
        return None;
    }
    let diagram = answer["diagram"]["mermaid"]
        .as_str()
        .filter(|mermaid| !mermaid.trim().is_empty())
        .map(|mermaid| Diagram {
            mermaid: mermaid.trim().to_string(),
            caption: answer["diagram"]["caption"]
                .as_str()
                .unwrap_or_default()
                .trim()
                .to_string(),
        });
    Some(Written {
        text,
        diagram,
        meaning_changed: answer["meaning_changed"].as_bool().unwrap_or(true),
    })
}

/// A writer's text and diagram given back to it with what's wrong.
pub fn fix_message(task: &str, written: &Written, problems: &[String], summary: bool) -> String {
    let key = if summary { "summary_md" } else { "body_md" };
    let diagram = written.diagram.as_ref().map_or_else(
        || "(none)".to_string(),
        |diagram| format!("{}\n\nIts caption: {}", diagram.mermaid, diagram.caption),
    );
    format!(
        "{task}\n\nThe problems found:\n{}\n\nYour {key}:\n\n{}\n\nYour diagram:\n\n{diagram}",
        problems
            .iter()
            .map(|problem| format!("- {problem}"))
            .collect::<Vec<_>>()
            .join("\n"),
        written.text,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wiki::check;

    #[test]
    fn the_example_s_diagram_reads_as_every_writer_s_must() {
        let mermaid: String = EXAMPLE
            .split("Its diagram:\n\n")
            .nth(1)
            .and_then(|rest| rest.split("\n\nIts caption").next())
            .unwrap()
            .to_string();
        let diagram = Diagram {
            mermaid,
            caption: String::new(),
        };
        let checked = check::diagram(&diagram).unwrap().mermaid;
        assert!(
            checked.contains("retrier[\"Retrier.Handle<br/>(internal/queue/retry.go)\"]"),
            "{checked}"
        );
    }

    #[test]
    fn an_answer_is_read_with_its_diagram_or_without() {
        let answer = json!({"body_md": " Text. ", "diagram": {"mermaid": "flowchart TD\n a --> b", "caption": "C."}});
        let written = read(&answer).unwrap();
        assert_eq!(written.text, "Text.");
        assert_eq!(written.diagram.unwrap().caption, "C.");
        assert!(written.meaning_changed);
        let none = json!({"summary_md": "S.", "diagram": {"mermaid": " ", "caption": ""}, "meaning_changed": false});
        let written = read(&none).unwrap();
        assert_eq!((written.diagram, written.meaning_changed), (None, false));
        assert_eq!(read(&json!({"body_md": ""})), None);
    }

    #[test]
    fn the_schema_asks_for_the_text_the_diagram_and_for_an_update_its_meaning() {
        let asked = schema(true, true);
        assert_eq!(
            asked["required"],
            json!(["summary_md", "diagram", "meaning_changed"])
        );
        assert_eq!(
            schema(false, false)["required"],
            json!(["body_md", "diagram"])
        );
        assert!(subsection_system().contains("Dead Letters and Replays"));
    }
}
