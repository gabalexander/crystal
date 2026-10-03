//! What [`render`] promises whatever it's given: no row wider than asked, a
//! ladder of fallbacks before giving up, no panic on any input, and, for
//! every diagram under `tests/mermaid/`, the drawing committed beside it,
//! character for character.
//!
//! A change to a layout shows up as a diff a reviewer can read. When one is
//! meant, write the drawings again and read the diff:
//!
//! ```text
//! CRYSTAL_MERMAID_UPDATE=1 cargo test mermaid
//! git diff tests/mermaid
//! ```

use super::*;
use std::path::{Path, PathBuf};

/// Every diagram drawn at 80 columns, and these again: narrow, wide, and
/// in ASCII.
const ALSO: &[(&str, usize, Glyphs)] = &[
    ("seq_request_path", 40, Glyphs::BOX_DRAWING),
    ("seq_long_labels", 40, Glyphs::BOX_DRAWING),
    ("seq_long_labels", 120, Glyphs::BOX_DRAWING),
    ("flow_daemon_modules", 40, Glyphs::BOX_DRAWING),
    ("flow_td_decision", 80, Glyphs::ASCII),
    ("seq_arrows", 80, Glyphs::ASCII),
    ("state_agent_status", 40, Glyphs::BOX_DRAWING),
    ("flow_lr_shapes", 40, Glyphs::BOX_DRAWING),
];

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/mermaid")
}

/// Every diagram under `tests/mermaid/`, by name, with its source.
fn fixtures() -> Vec<(String, String)> {
    let mut found: Vec<(String, String)> = std::fs::read_dir(fixtures_dir())
        .expect("the diagrams")
        .filter_map(|entry| Some(entry.ok()?.path()))
        .filter(|path| path.extension().is_some_and(|x| x == "mmd"))
        .map(|path| {
            let name = path.file_stem().unwrap().to_string_lossy().into_owned();
            (name, std::fs::read_to_string(&path).unwrap())
        })
        .collect();
    found.sort();
    found
}

fn lines(rendered: &Rendered) -> &[String] {
    match rendered {
        Rendered::Diagram { lines, .. } => lines,
        Rendered::Unsupported { reason } => panic!("not drawn: {reason}"),
    }
}

fn reason(rendered: Rendered) -> String {
    match rendered {
        Rendered::Unsupported { reason } => reason,
        drawn => panic!("drawn: {drawn:?}"),
    }
}

fn draw(source: &str, width: usize) -> Rendered {
    render(source, width, Glyphs::BOX_DRAWING)
}

/// The drawing of `source` as it's committed: its kind and width, then
/// its rows, or why it wasn't drawn.
fn snapshot(source: &str, width: usize, glyphs: Glyphs) -> String {
    match render(source, width, glyphs) {
        Rendered::Diagram { lines, kind } => {
            for line in &lines {
                assert!(
                    display_width(line) <= width,
                    "a row is {} columns at {width}:\n{line}",
                    display_width(line)
                );
            }
            format!("{} · width {width}\n{}\n", kind.label(), lines.join("\n"))
        }
        Rendered::Unsupported { reason } => format!("unsupported · width {width}\n{reason}\n"),
    }
}

#[test]
fn every_diagram_draws_as_committed() {
    let update = std::env::var_os("CRYSTAL_MERMAID_UPDATE").is_some();
    let fixtures = fixtures();
    assert!(fixtures.len() >= 15, "only {} diagrams", fixtures.len());
    let cases = fixtures
        .iter()
        .map(|(name, _)| (name.as_str(), 80, Glyphs::BOX_DRAWING))
        .chain(ALSO.iter().copied());
    let mut differ = Vec::new();
    for (name, width, glyphs) in cases {
        let source = std::fs::read_to_string(fixtures_dir().join(format!("{name}.mmd"))).unwrap();
        let drawn = snapshot(&source, width, glyphs);
        let file = match (width, glyphs.ascii) {
            (80, false) => format!("{name}.txt"),
            (width, false) => format!("{name}.w{width}.txt"),
            (width, true) => format!("{name}.ascii.w{width}.txt"),
        };
        let path = fixtures_dir().join(&file);
        if update {
            std::fs::write(&path, &drawn).unwrap();
            continue;
        }
        let committed = std::fs::read_to_string(&path).unwrap_or_default();
        if committed != drawn {
            differ.push(format!(
                "--- {file} (committed)\n{committed}+++ (drawn)\n{drawn}"
            ));
        }
    }
    assert!(
        differ.is_empty(),
        "{} drawings differ; CRYSTAL_MERMAID_UPDATE=1 writes them again:\n{}",
        differ.len(),
        differ.join("\n")
    );
}

#[test]
fn the_kind_comes_from_the_first_statement() {
    assert_eq!(
        detect("sequenceDiagram\nA->>B: x"),
        Some(Ok(Kind::Sequence))
    );
    assert_eq!(
        detect("%% a comment\n\n  graph LR;A-->B"),
        Some(Ok(Kind::Flowchart))
    );
    assert_eq!(
        detect("---\ntitle: Orders\n---\nerDiagram\n"),
        Some(Ok(Kind::Er))
    );
    assert_eq!(detect("stateDiagram-v2\n"), Some(Ok(Kind::State)));
    assert_eq!(detect("classDiagram\n"), Some(Ok(Kind::Class)));
    assert_eq!(detect("gantt\n title x"), Some(Err("gantt".into())));
    assert_eq!(detect("  \n%% only a comment\n"), None);
}

#[test]
fn other_kinds_and_empty_sources_arent_drawn_and_say_why() {
    for (source, why) in [
        ("pie\n \"a\": 1", "pie diagrams are not drawn in a terminal"),
        (
            "gitGraph\n commit",
            "gitGraph diagrams are not drawn in a terminal",
        ),
        (
            "mindmap\n root",
            "mindmap diagrams are not drawn in a terminal",
        ),
        ("", "the diagram is empty"),
    ] {
        assert_eq!(reason(draw(source, 80)), why, "{source}");
    }
}

/// What a page relies on: every row fits the width, at the widths a page
/// is often drawn at, with either set of glyphs.
#[test]
fn no_row_is_ever_wider_than_the_width_at_40_80_and_120() {
    let mut drawn = 0;
    for (name, source) in fixtures() {
        for width in [40, 80, 120] {
            for glyphs in [Glyphs::BOX_DRAWING, Glyphs::ASCII] {
                match render(&source, width, glyphs) {
                    Rendered::Diagram { lines, .. } => {
                        drawn += 1;
                        for line in &lines {
                            assert!(
                                display_width(line) <= width,
                                "{name} at {width}: {} columns\n{}",
                                display_width(line),
                                lines.join("\n")
                            );
                        }
                    }
                    Rendered::Unsupported { reason } => {
                        assert!(width < 80, "{name} refused at {width}: {reason}");
                    }
                }
            }
        }
    }
    assert!(drawn > 80, "only {drawn} drawings drew anything");
}

/// Every diagram draws at 120 and at 80, the widths a preview has, as the
/// kind its name says.
#[test]
fn every_diagram_draws_at_80_and_120_as_its_kind() {
    for (name, source) in fixtures() {
        for width in [80, 120] {
            let Rendered::Diagram { kind, .. } = draw(&source, width) else {
                panic!("{name} at {width} isn't drawn");
            };
            let expected = match name.split('_').next().unwrap() {
                "seq" => Kind::Sequence,
                "flow" => Kind::Flowchart,
                "state" => Kind::State,
                "class" => Kind::Class,
                "er" => Kind::Er,
                other => panic!("a diagram named for {other}"),
            };
            assert_eq!(kind, expected, "{name}");
        }
    }
}

const WIDE_SEQUENCE: &str = "sequenceDiagram
    participant Client
    participant Gateway
    participant Orders
    Client->>Gateway: POST /orders with the whole basket attached
    Gateway->>Orders: create the order in the pending state
    Orders-->>Client: 201 Created";

/// Wider than the page: first the boxes shrink to their names, then the
/// labels are cut with `…`, and past that the diagram is refused.
#[test]
fn a_sequence_shrinks_its_boxes_then_cuts_its_labels_then_gives_up() {
    let roomy = draw(WIDE_SEQUENCE, 120);
    let text = lines(&roomy).join("\n");
    assert!(text.contains("│ Client │"), "padded boxes:\n{text}");
    assert!(!text.contains('…'));

    // Just too narrow for the padded boxes: they shrink, the words stay.
    let full = lines(&roomy)
        .iter()
        .map(|line| display_width(line))
        .max()
        .unwrap();
    let shrunk = draw(WIDE_SEQUENCE, full - 2);
    let text = lines(&shrunk).join("\n");
    assert!(text.contains("│Client│"), "boxes shrunk:\n{text}");
    assert!(
        text.contains("POST /orders with the whole basket attached"),
        "{text}"
    );

    let cut = draw(WIDE_SEQUENCE, 50);
    let text = lines(&cut).join("\n");
    assert!(text.contains('…'), "labels cut short:\n{text}");
    assert!(text.contains("│Client│"));

    let why = reason(draw(WIDE_SEQUENCE, 20));
    assert!(why.contains("3 participants"), "{why}");
    assert!(why.contains("20 available"), "{why}");
}

/// A flowchart cuts its labels before giving up, and turns `LR` into `TD`
/// when a chain is simply too long for the width.
#[test]
fn a_flowchart_cuts_its_labels_turns_and_then_gives_up() {
    let source = "flowchart TD\n a[a label that is much too long to fit in the width] --> b[short]";
    let text = lines(&draw(source, 30)).join("\n");
    assert!(text.contains('…') && text.contains("short"), "{text}");

    let chain = "flowchart LR\n one --> two --> three --> four --> five --> six --> seven";
    assert_eq!(lines(&draw(chain, 120)).len(), 3, "one row of boxes");
    assert!(lines(&draw(chain, 40)).len() > 10, "turned top down");

    let fan: String = std::iter::once("flowchart TD".to_string())
        .chain((0..12).map(|i| format!(" root --> child_number_{i}")))
        .collect::<Vec<_>>()
        .join("\n");
    let why = reason(draw(&fan, 12));
    assert!(why.contains("12 available"), "{why}");
}

#[test]
fn forty_nodes_are_drawn_and_forty_one_are_refused() {
    let chain = |nodes: usize| {
        let mut source = String::from("flowchart TD\n");
        for i in 0..nodes - 1 {
            source.push_str(&format!(" n{i} --> n{}\n", i + 1));
        }
        source
    };
    assert!(matches!(draw(&chain(40), 80), Rendered::Diagram { .. }));
    let why = reason(draw(&chain(41), 80));
    assert!(why.contains("41 nodes"), "{why}");
}

/// The worst a model can write inside the limits (forty nodes in a chain,
/// and the rest of the hundred edges each spanning the whole chain, a
/// dummy node in every layer) is laid out in a bounded time, the whole
/// ladder of fallbacks included; one edge past the limit is refused before
/// any layout.
#[test]
fn a_dense_graph_is_bounded_in_time_and_edges() {
    let mut source = String::from("flowchart TD\n");
    for i in 0..39 {
        source.push_str(&format!(" n{i} --> n{}\n", i + 1));
    }
    for k in 0..61 {
        source.push_str(&format!(" n{} --> n{}\n", k % 5, 35 + k % 5));
    }
    let started = std::time::Instant::now();
    let rendered = draw(&source, 80);
    let took = started.elapsed();
    // A test build isn't optimized: give it ten times the room.
    let most = if cfg!(debug_assertions) { 5000 } else { 500 };
    assert!(
        took < std::time::Duration::from_millis(most),
        "100 edges took {took:?}"
    );
    if let Rendered::Diagram { lines, .. } = &rendered {
        assert!(lines.iter().all(|line| display_width(line) <= 80));
    }
    source.push_str(" n0 --> n2\n");
    let why = reason(draw(&source, 80));
    assert!(why.contains("101 edges"), "{why}");
}

#[test]
fn ascii_is_ascii_all_the_way_through() {
    for (name, source) in fixtures() {
        if name.contains("cjk") {
            continue;
        }
        if let Rendered::Diagram { lines, .. } = render(&source, 120, Glyphs::ASCII) {
            for line in &lines {
                assert!(line.is_ascii(), "{name}: {line}");
            }
        }
    }
}

/// Wide characters take two columns, and the boxes around them are drawn
/// to match: every row of a box is as wide on screen as the others.
#[test]
fn boxes_around_wide_labels_line_up() {
    let rendered = draw("flowchart TD\n a[日本語] --> b[ok]", 40);
    let rows = lines(&rendered);
    assert_eq!(rows[1].trim(), "│ 日本語 │");
    assert_eq!(display_width(&rows[0]), display_width(&rows[1]));
    assert_eq!(display_width(&rows[2]), display_width(&rows[1]));
    let sequence = draw("sequenceDiagram\n 客户->>服务: 请求", 40);
    for line in lines(&sequence) {
        assert!(display_width(line) <= 40);
    }
}

/// Random inputs (random bytes, random tokens, and the diagrams here with
/// characters changed) through every parser and layout, at three widths.
/// None may panic, and whatever is drawn must fit.
/// `CRYSTAL_MERMAID_FUZZ=200000` runs longer.
#[test]
fn the_parsers_never_panic_on_garbage() {
    let mut state: u64 = 0x2545_F491_4F6C_DD1D;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let seeds = fixtures();
    let heads = [
        "flowchart TD\n",
        "graph LR\n",
        "sequenceDiagram\n",
        "stateDiagram-v2\n",
        "classDiagram\n",
        "erDiagram\n",
    ];
    let alphabet: Vec<char> = "AB-->|[](){}:;\"<>x.=&%*\n o|{}~+,日".chars().collect();
    let rounds: usize = std::env::var("CRYSTAL_MERMAID_FUZZ")
        .ok()
        .and_then(|rounds| rounds.parse().ok())
        .unwrap_or(600);
    for round in 0..rounds {
        let head = heads[(next() as usize) % heads.len()];
        let source: String = match round % 3 {
            0 => {
                let bytes: Vec<u8> = (0..(next() % 120)).map(|_| next() as u8).collect();
                format!("{head}{}", String::from_utf8_lossy(&bytes))
            }
            1 => {
                let mut source = head.to_string();
                for _ in 0..(next() % 100) {
                    source.push(alphabet[(next() as usize) % alphabet.len()]);
                }
                source
            }
            _ => {
                let seed = &seeds[(next() as usize) % seeds.len()].1;
                let mut chars: Vec<char> = seed.chars().collect();
                for _ in 0..(1 + next() % 10) {
                    if chars.is_empty() {
                        break;
                    }
                    let at = (next() as usize) % chars.len();
                    let letter = alphabet[(next() as usize) % alphabet.len()];
                    match next() % 3 {
                        0 => {
                            chars.remove(at);
                        }
                        1 => chars.insert(at, letter),
                        _ => chars[at] = letter,
                    }
                }
                chars.into_iter().collect()
            }
        };
        for width in [1, 17, 80] {
            if let Rendered::Diagram { lines, .. } = draw(&source, width) {
                for line in &lines {
                    assert!(
                        display_width(line) <= width,
                        "{source:?} at {width}: {line}"
                    );
                }
            }
        }
    }
}
