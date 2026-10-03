//! `classDiagram` read into a [`Graph`]: every class a box with its name
//! on top, its fields under a rule and its methods under another, every
//! relation an edge whose ends carry mermaid's markers (`△` for inheritance
//! and realisation, `◆` composition, `◇` aggregation, an arrow for
//! association and dependency, dotted for the `..` forms) and whose label
//! spells the cardinalities (`1—*`) before the relation's name. Laid out
//! top down, parents above children, by the layered engine.
//!
//! `namespace X { … }` is a frame; `<<interface>>` and the other
//! annotations are a line over the name; generics `List~T~` read `List<T>`.
//! Notes, styling and click handlers are read and dropped.

use super::canvas::Stroke;
use super::flowchart::clean;
use super::graph::{Dir, Graph, Mark, Row};

/// A class as it is read: annotations, fields and methods, turned into
/// box rows once the whole source is in.
#[derive(Default)]
struct Members {
    annotations: Vec<String>,
    label: Option<String>,
    fields: Vec<String>,
    methods: Vec<String>,
}

const IGNORED: &[&str] = &[
    "note", "classDef", "style", "cssClass", "click", "link", "callback", "accTitle", "accDescr",
    "title",
];

pub fn parse(src: &str) -> Result<Graph, String> {
    let mut lines = super::lines(src);
    lines.next();
    let mut g = Graph::new(Dir::Down);
    let mut members: Vec<Members> = Vec::new();
    let mut body: Option<usize> = None;
    let mut ns: Vec<usize> = Vec::new();
    let touch = |g: &mut Graph, members: &mut Vec<Members>, id: &str, group: Option<usize>| {
        let n = g.node(id);
        if members.len() <= n {
            members.resize_with(n + 1, Members::default);
        }
        g.assign(n, group);
        n
    };
    for (no, line) in lines {
        let group = ns.last().copied();
        if let Some(n) = body {
            if line == "}" {
                body = None;
                continue;
            }
            let m = member_text(line);
            if let Some(a) = annotation(line) {
                members[n].annotations.push(a);
            } else if m.contains('(') {
                members[n].methods.push(m);
            } else {
                members[n].fields.push(m);
            }
            continue;
        }
        let first = line.split_whitespace().next().unwrap_or("");
        if IGNORED.contains(&first) {
            continue;
        }
        if first == "direction" {
            if let Some(d) = line.split_whitespace().nth(1).and_then(Dir::parse) {
                g.dir = d;
            }
            continue;
        }
        if first == "namespace" {
            let name = line["namespace".len()..]
                .trim()
                .trim_end_matches('{')
                .trim();
            ns.push(g.add_group(name, group));
            continue;
        }
        if line == "}" {
            if ns.pop().is_none() {
                return Err(format!("line {no}: `}}` without a class or namespace"));
            }
            continue;
        }
        if first == "class" {
            let rest = line["class".len()..].trim();
            let (head, open) = match rest.strip_suffix('{') {
                Some(h) => (h.trim(), true),
                None => (rest, false),
            };
            let (head, label) = match head.find('[') {
                Some(i) => (
                    head[..i].trim(),
                    Some(clean(head[i + 1..].trim_end_matches(']'))),
                ),
                None => (head, None),
            };
            let head = head.split(":::").next().unwrap_or(head).trim();
            if head.is_empty() || head.contains(char::is_whitespace) {
                return Err(format!("line {no}: cannot read the class `{head}`"));
            }
            let n = touch(&mut g, &mut members, head, group);
            if label.is_some() {
                members[n].label = label;
            }
            if open {
                body = Some(n);
            }
            continue;
        }
        if let Some(a) = annotation(line) {
            // `<<interface>> Name`
            let name = line.rsplit(">>").next().unwrap_or("").trim();
            if name.is_empty() {
                return Err(format!("line {no}: an annotation names no class"));
            }
            let n = touch(&mut g, &mut members, name, group);
            members[n].annotations.push(a);
            continue;
        }
        if let Some(rel) = relation(line) {
            let (a, b) = (
                touch(&mut g, &mut members, &rel.left, group),
                touch(&mut g, &mut members, &rel.right, group),
            );
            let cards = match (&rel.left_card, &rel.right_card) {
                (None, None) => None,
                (l, r) => Some(format!(
                    "{}—{}",
                    l.as_deref().unwrap_or(""),
                    r.as_deref().unwrap_or("")
                )),
            };
            let label = match (cards, rel.label) {
                (Some(c), Some(l)) => Some(format!("{c} {l}")),
                (Some(c), None) => Some(c),
                (None, l) => l,
            };
            g.edge(a, b, label, rel.stroke);
            let e = g.edges.last_mut().expect("just pushed");
            e.from_mark = rel.left_mark;
            e.to_mark = rel.right_mark;
            continue;
        }
        if let Some((name, member)) = line.split_once(':') {
            let name = name.trim();
            if name.is_empty() || name.contains(char::is_whitespace) {
                return Err(format!("line {no}: cannot read `{line}`"));
            }
            let n = touch(&mut g, &mut members, name, group);
            let m = member_text(member);
            if m.contains('(') {
                members[n].methods.push(m);
            } else {
                members[n].fields.push(m);
            }
            continue;
        }
        let name = line.trim();
        if name.contains(char::is_whitespace) {
            return Err(format!("line {no}: cannot read `{line}`"));
        }
        touch(&mut g, &mut members, name, group);
    }
    if body.is_some() {
        return Err("a class body is never closed with `}`".into());
    }
    for (n, m) in members.into_iter().enumerate() {
        if n >= g.nodes.len() {
            break;
        }
        let mut rows: Vec<Row> = m
            .annotations
            .iter()
            .map(|a| Row::Text(format!("«{a}»")))
            .collect();
        let name = m.label.unwrap_or_else(|| generics(&g.nodes[n].id));
        rows.push(Row::Text(name));
        if !m.fields.is_empty() || !m.methods.is_empty() {
            rows.push(Row::Rule);
            rows.extend(m.fields.into_iter().map(Row::Text));
        }
        if !m.methods.is_empty() {
            if rows.last() != Some(&Row::Rule) {
                rows.push(Row::Rule);
            }
            rows.extend(m.methods.into_iter().map(Row::Text));
        }
        g.nodes[n].rows = rows;
    }
    Ok(g)
}

/// `<<interface>>` → `interface`.
fn annotation(line: &str) -> Option<String> {
    let rest = line.trim().strip_prefix("<<")?;
    let end = rest.find(">>")?;
    Some(rest[..end].trim().to_string())
}

/// `List~T~` → `List<T>`.
fn generics(s: &str) -> String {
    let mut out = String::new();
    let mut open = true;
    for c in s.chars() {
        if c == '~' {
            out.push(if open { '<' } else { '>' });
            open = !open;
        } else {
            out.push(c);
        }
    }
    out
}

/// A member line as shown: generics spelled out, the `$` and `*`
/// classifiers (static, abstract) dropped.
fn member_text(s: &str) -> String {
    generics(s.trim().trim_end_matches(['$', '*']))
}

struct Relation {
    left: String,
    right: String,
    left_card: Option<String>,
    right_card: Option<String>,
    left_mark: Mark,
    right_mark: Mark,
    stroke: Stroke,
    label: Option<String>,
}

/// `A "1" *-- "many" B : label` and every arrow between.
fn relation(line: &str) -> Option<Relation> {
    let (body, label) = split_label(line);
    // The operator: the run holding `--` or `..` and its end markers.
    let bytes = body.as_bytes();
    let at = body.find("--").or_else(|| body.find(".."))?;
    let mut s = at;
    while s > 0 && matches!(bytes[s - 1], b'<' | b'|' | b'*' | b'o' | b'-' | b'.') {
        // `o` is a marker only when a space or a quote stands before it.
        if bytes[s - 1] == b'o' && s >= 2 && !matches!(bytes[s - 2], b' ' | b'"') {
            break;
        }
        s -= 1;
    }
    let mut e = at;
    while e < bytes.len() && matches!(bytes[e], b'>' | b'|' | b'*' | b'o' | b'-' | b'.') {
        if bytes[e] == b'o' && e + 1 < bytes.len() && !matches!(bytes[e + 1], b' ' | b'"') {
            break;
        }
        e += 1;
    }
    let op = &body[s..e];
    let (lhs, rhs) = (&body[..s], &body[e..]);
    let (left, left_card) = side(lhs, true)?;
    let (right, right_card) = side(rhs, false)?;
    let stroke = if op.contains("..") {
        Stroke::Dotted
    } else {
        Stroke::Solid
    };
    let core = op.trim_matches(|c| c == '-' || c == '.');
    let (lm, rm) = if op.starts_with(['-', '.']) {
        ("", core)
    } else if op.ends_with(['-', '.']) {
        (core, "")
    } else {
        let split = op.find(['-', '.']).unwrap_or(0);
        let rsplit = op.rfind(['-', '.']).map(|i| i + 1).unwrap_or(op.len());
        (&op[..split], &op[rsplit..])
    };
    let mark = |m: &str| match m {
        "<|" | "|>" => Mark::Triangle,
        "*" => Mark::Diamond,
        "o" => Mark::Hollow,
        "<" | ">" => Mark::Arrow,
        _ => Mark::None,
    };
    Some(Relation {
        left,
        right,
        left_card,
        right_card,
        left_mark: mark(lm),
        right_mark: mark(rm),
        stroke,
        label,
    })
}

/// The part before the first `:` outside quotes, and the label after it.
fn split_label(line: &str) -> (&str, Option<String>) {
    let mut quoted = false;
    for (i, c) in line.char_indices() {
        match c {
            '"' => quoted = !quoted,
            ':' if !quoted => {
                let l = clean(&line[i + 1..]);
                return (&line[..i], (!l.is_empty()).then_some(l));
            }
            _ => {}
        }
    }
    (line, None)
}

/// One side of a relation: the class and, beside the operator, a quoted
/// cardinality.
fn side(s: &str, left: bool) -> Option<(String, Option<String>)> {
    let s = s.trim();
    let (name, card) = if left {
        match s.strip_suffix('"') {
            Some(rest) => {
                let q = rest.rfind('"')?;
                (rest[..q].trim(), Some(rest[q + 1..].to_string()))
            }
            None => (s, None),
        }
    } else {
        match s.strip_prefix('"') {
            Some(rest) => {
                let q = rest.find('"')?;
                (rest[q + 1..].trim(), Some(rest[..q].to_string()))
            }
            None => (s, None),
        }
    };
    if name.is_empty() || name.contains(char::is_whitespace) {
        return None;
    }
    Some((name.to_string(), card))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_class_body_splits_fields_from_methods_under_rules() {
        let g = parse(
            "classDiagram\n class Tour {\n  <<entity>>\n  +String id\n  +add(stop) Stop\n  +List~Stop~ stops\n }",
        )
        .unwrap();
        assert_eq!(
            g.nodes[0].rows,
            vec![
                Row::Text("«entity»".into()),
                Row::Text("Tour".into()),
                Row::Rule,
                Row::Text("+String id".into()),
                Row::Text("+List<Stop> stops".into()),
                Row::Rule,
                Row::Text("+add(stop) Stop".into()),
            ]
        );
    }

    #[test]
    fn members_after_a_colon_and_annotations_on_their_own_line() {
        let g =
            parse("classDiagram\n Duck : +swim()\n Duck : +int age\n <<interface>> Duck").unwrap();
        assert_eq!(
            g.nodes[0].rows,
            vec![
                Row::Text("«interface»".into()),
                Row::Text("Duck".into()),
                Row::Rule,
                Row::Text("+int age".into()),
                Row::Rule,
                Row::Text("+swim()".into()),
            ]
        );
    }

    #[test]
    fn every_relation_marks_its_ends() {
        let g = parse(
            "classDiagram\n A <|-- B\n C *-- D\n E o-- F\n G --> H\n I ..> J\n K ..|> L\n M -- N\n O .. P",
        )
        .unwrap();
        let ends: Vec<(Mark, Mark, Stroke)> = g
            .edges
            .iter()
            .map(|e| (e.from_mark, e.to_mark, e.stroke))
            .collect();
        use Mark::*;
        assert_eq!(
            ends,
            vec![
                (Triangle, None, Stroke::Solid),
                (Diamond, None, Stroke::Solid),
                (Hollow, None, Stroke::Solid),
                (None, Arrow, Stroke::Solid),
                (None, Arrow, Stroke::Dotted),
                (None, Triangle, Stroke::Dotted),
                (None, None, Stroke::Solid),
                (None, None, Stroke::Dotted),
            ]
        );
    }

    #[test]
    fn cardinalities_and_labels_become_the_edge_label() {
        let g = parse("classDiagram\n Customer \"1\" --> \"*\" Ticket : buys\n A \"0..1\" -- B")
            .unwrap();
        assert_eq!(g.edges[0].label.as_deref(), Some("1—* buys"));
        assert_eq!(g.edges[1].label.as_deref(), Some("0..1—"));
        assert_eq!(
            g.nodes[g.find("Ticket").unwrap()].rows,
            vec![Row::Text("Ticket".into())]
        );
    }

    #[test]
    fn namespaces_are_frames_and_garbage_is_an_error() {
        let g = parse("classDiagram\n namespace Shapes {\n  class Square\n  class Circle\n }\n Square <|-- Circle").unwrap();
        assert_eq!(g.groups.len(), 1);
        assert_eq!(g.nodes[0].group, Some(0));
        assert!(parse("classDiagram\n this is not a class").is_err());
        assert!(parse("classDiagram\n class A {\n  +x").is_err());
    }
}
