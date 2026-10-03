//! `erDiagram` read into a [`Graph`]: every entity a box with its name
//! over a rule and its attributes under it (`type name PK`), every
//! relationship an edge labelled with its cardinalities written out
//! (`||--o{` reads `1—*`) and its name, dotted for a non-identifying (`..`)
//! one. Crow's feet are pictures, and a terminal cell holds a character, so
//! the notation is spelled instead: `1` exactly one, `0..1` zero or one,
//! `*` zero or more, `1..*` one or more.

use super::canvas::Stroke;
use super::flowchart::clean;
use super::graph::{Dir, Graph, Mark, Row};

pub fn parse(src: &str) -> Result<Graph, String> {
    let mut lines = super::lines(src);
    lines.next();
    let mut g = Graph::new(Dir::Down);
    let mut attrs: Vec<Vec<String>> = Vec::new();
    let mut labels: Vec<Option<String>> = Vec::new();
    let mut body: Option<usize> = None;
    let entity = |g: &mut Graph,
                  attrs: &mut Vec<Vec<String>>,
                  labels: &mut Vec<Option<String>>,
                  raw: &str| {
        let (id, label) = match raw.find('[') {
            Some(i) => (
                raw[..i].trim(),
                Some(clean(raw[i + 1..].trim_end_matches(']'))),
            ),
            None => (raw.trim(), None),
        };
        let n = g.node(id);
        if attrs.len() <= n {
            attrs.resize_with(n + 1, Vec::new);
            labels.resize_with(n + 1, || None);
        }
        if label.is_some() {
            labels[n] = label;
        }
        n
    };
    for (no, line) in lines {
        if let Some(n) = body {
            if line == "}" {
                body = None;
                continue;
            }
            attrs[n].push(attribute(line));
            continue;
        }
        let first = line.split_whitespace().next().unwrap_or("");
        if matches!(
            first,
            "direction" | "title" | "accTitle" | "accDescr" | "style" | "classDef" | "class"
        ) {
            if first == "direction"
                && let Some(d) = line.split_whitespace().nth(1).and_then(Dir::parse)
            {
                g.dir = d;
            }
            continue;
        }
        if let Some(head) = line.strip_suffix('{') {
            let head = head.trim();
            if head.is_empty() || head.contains(char::is_whitespace) && !head.contains('[') {
                return Err(format!("line {no}: cannot read the entity `{head}`"));
            }
            body = Some(entity(&mut g, &mut attrs, &mut labels, head));
            continue;
        }
        let (rel, label) = match line.split_once(':') {
            Some((r, l)) => (r.trim(), Some(clean(l))),
            None => (line, None),
        };
        let words: Vec<&str> = rel.split_whitespace().collect();
        match words.as_slice() {
            [a, op, b] => {
                let (lc, stroke, rc) = cardinality(op)
                    .ok_or_else(|| format!("line {no}: cannot read the relationship `{op}`"))?;
                let a = entity(&mut g, &mut attrs, &mut labels, a);
                let b = entity(&mut g, &mut attrs, &mut labels, b);
                let text = match label.filter(|l| !l.is_empty()) {
                    Some(l) => format!("{lc}—{rc} {l}"),
                    None => format!("{lc}—{rc}"),
                };
                g.edge(a, b, Some(text), stroke);
                let e = g.edges.last_mut().expect("just pushed");
                e.to_mark = Mark::None;
            }
            [a] if label.is_none() => {
                entity(&mut g, &mut attrs, &mut labels, a);
            }
            _ => return Err(format!("line {no}: cannot read `{line}`")),
        }
    }
    if body.is_some() {
        return Err("an entity's attributes are never closed with `}`".into());
    }
    for n in 0..g.nodes.len() {
        let name = labels
            .get(n)
            .cloned()
            .flatten()
            .unwrap_or_else(|| g.nodes[n].id.clone());
        let mut rows = vec![Row::Text(name)];
        if let Some(a) = attrs.get(n).filter(|a| !a.is_empty()) {
            rows.push(Row::Rule);
            rows.extend(a.iter().cloned().map(Row::Text));
        }
        g.nodes[n].rows = rows;
    }
    Ok(g)
}

/// `string email PK "the login"` → `string email PK`.
fn attribute(line: &str) -> String {
    let line = match line.find('"') {
        Some(i) => &line[..i],
        None => line,
    };
    line.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace(" ,", ",")
}

/// `||--o{` → (`1`, solid, `*`).
fn cardinality(op: &str) -> Option<(&'static str, Stroke, &'static str)> {
    if op.len() < 6 || !op.is_ascii() {
        return None;
    }
    let (l, mid, r) = (&op[..2], &op[2..op.len() - 2], &op[op.len() - 2..]);
    let stroke = match mid {
        "--" => Stroke::Solid,
        ".." => Stroke::Dotted,
        _ => return None,
    };
    let left = match l {
        "||" => "1",
        "|o" => "0..1",
        "}o" => "*",
        "}|" => "1..*",
        _ => return None,
    };
    let right = match r {
        "||" => "1",
        "o|" => "0..1",
        "o{" => "*",
        "|{" => "1..*",
        _ => return None,
    };
    Some((left, stroke, right))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relationships_spell_their_cardinalities() {
        let g = parse(
            "erDiagram\n A ||--o{ B : places\n B }|..|{ C : \"is in\"\n C |o--o| D\n D }o--|| E : x",
        )
        .unwrap();
        let labels: Vec<&str> = g.edges.iter().filter_map(|e| e.label.as_deref()).collect();
        assert_eq!(
            labels,
            vec!["1—* places", "1..*—1..* is in", "0..1—0..1", "*—1 x"]
        );
        assert_eq!(g.edges[1].stroke, Stroke::Dotted);
        assert!(g.edges.iter().all(|e| e.to_mark == Mark::None));
    }

    #[test]
    fn entity_blocks_list_their_attributes_under_a_rule() {
        let g = parse(
            "erDiagram\n CUSTOMER {\n  string name\n  string email PK \"the login\"\n  int a PK, FK\n }\n CUSTOMER ||--|| X : has",
        )
        .unwrap();
        assert_eq!(
            g.nodes[0].rows,
            vec![
                Row::Text("CUSTOMER".into()),
                Row::Rule,
                Row::Text("string name".into()),
                Row::Text("string email PK".into()),
                Row::Text("int a PK, FK".into()),
            ]
        );
        assert_eq!(g.nodes[1].rows, vec![Row::Text("X".into())]);
    }

    #[test]
    fn unknown_notation_is_an_error() {
        assert!(parse("erDiagram\n A ||~~o{ B : x").is_err());
        assert!(parse("erDiagram\n A only one to B").is_err());
        assert!(parse("erDiagram\n A {\n int x").is_err());
    }
}
