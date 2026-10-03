//! `flowchart` / `graph` source read into a [`Graph`].
//!
//! The common subset, the part a model writes when it explains control
//! flow: a direction; nodes with an id and an optional label in one of the
//! shape brackets; edges `-->`, `---`, `-.->`, `==>` (and their longer and
//! crossed/circled variants) labelled `|like this|` or `-- like this -->`;
//! `&` to fan out; chains `A --> B --> C`; `subgraph … end`, nested too;
//! `%%` comments. Styling lines (`classDef`, `class`, `style`,
//! `linkStyle`, `click`) are read and dropped: colour is the one thing a
//! text diagram can't keep. Anything else is an error naming the line,
//! because a diagram drawn with a statement silently missing is worse than
//! the source shown as written.

use super::canvas::Stroke;
use super::graph::{Dir, Graph, Mark, Row, Shape};

/// Lines read and deliberately dropped.
const IGNORED: &[&str] = &[
    "classDef",
    "class",
    "style",
    "linkStyle",
    "click",
    "accTitle",
    "accDescr",
    "direction",
    "title",
];

/// Parse a flowchart. The header line (`flowchart LR`) is the first line.
pub fn parse(src: &str) -> Result<Graph, String> {
    let mut lines = super::lines(src)
        .flat_map(|(no, line)| split_statements(line).into_iter().map(move |s| (no, s)));
    let Some((_, header)) = lines.next() else {
        return Err("the diagram is empty".into());
    };
    let mut words = header.split_whitespace();
    words.next();
    let dir = match words.next() {
        Some(d) => {
            Dir::parse(d.trim_end_matches(';')).ok_or_else(|| format!("unknown direction `{d}`"))?
        }
        None => Dir::Down,
    };
    let mut g = Graph::new(dir);
    let mut stack: Vec<usize> = Vec::new();
    for (no, line) in lines {
        let first = line.split_whitespace().next().unwrap_or("");
        if first == "subgraph" {
            let rest = line["subgraph".len()..].trim();
            let (id, title) = subgraph_title(rest);
            let parent = stack.last().copied();
            let gid = g.add_group(&title, parent);
            let _ = id;
            stack.push(gid);
            continue;
        }
        if first == "end" && line.trim() == "end" {
            if stack.pop().is_none() {
                return Err(format!("line {no}: `end` without a subgraph"));
            }
            continue;
        }
        if IGNORED.contains(&first) || first.starts_with("%%") {
            continue;
        }
        statement(&mut g, line, stack.last().copied()).map_err(|e| format!("line {no}: {e}"))?;
    }
    Ok(g)
}

/// A line split at the `;` that ends a statement: outside quotes and
/// brackets, where it is part of a label.
fn split_statements(line: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut quoted = false;
    let mut start = 0;
    for (i, ch) in line.char_indices() {
        match ch {
            '"' => quoted = !quoted,
            '[' | '(' | '{' if !quoted => depth += 1,
            ']' | ')' | '}' if !quoted => depth -= 1,
            ';' if !quoted && depth <= 0 => {
                out.push(line[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(line[start..].trim());
    out.retain(|s| !s.is_empty());
    out
}

/// `id [title]`, `id["title"]`, `"title"`, or `a title with spaces`.
fn subgraph_title(rest: &str) -> (String, String) {
    let rest = rest.trim();
    if rest.is_empty() {
        return (String::new(), String::new());
    }
    if let Some(q) = rest.strip_prefix('"') {
        let t = q.split('"').next().unwrap_or("");
        return (t.to_string(), clean(t));
    }
    if let Some(open) = rest.find('[') {
        let id = rest[..open].trim();
        let inner = rest[open + 1..].trim_end().trim_end_matches(']');
        let inner = inner.trim().trim_matches('"');
        return (id.to_string(), clean(inner));
    }
    (rest.to_string(), clean(rest))
}

/// Label text as a reader should see it: quotes and markdown markers off,
/// `<br>` as a line break, mermaid's `#quot;`-style entities decoded.
pub fn clean(s: &str) -> String {
    let s = s.trim();
    let s = s
        .strip_prefix('"')
        .and_then(|x| x.strip_suffix('"'))
        .unwrap_or(s);
    let s = s
        .strip_prefix('`')
        .and_then(|x| x.strip_suffix('`'))
        .unwrap_or(s);
    let mut out = s
        .replace("<br/>", "\n")
        .replace("<br />", "\n")
        .replace("<br>", "\n")
        .replace("\\n", "\n")
        .replace("#quot;", "\"")
        .replace("#35;", "#")
        .replace("#59;", ";")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("**", "");
    if out.starts_with('*') && out.ends_with('*') && out.len() > 1 {
        out = out[1..out.len() - 1].to_string();
    }
    out
}

/// A label's lines as box rows.
pub fn rows(label: &str) -> Vec<Row> {
    label
        .split('\n')
        .map(|l| Row::Text(l.trim().to_string()))
        .collect()
}

struct Cursor<'a> {
    s: &'a [u8],
    src: &'a str,
    i: usize,
}

impl<'a> Cursor<'a> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }
    fn at(&self, k: usize) -> Option<u8> {
        self.s.get(self.i + k).copied()
    }
    fn rest(&self) -> &'a str {
        &self.src[self.i..]
    }
    fn skip_ws(&mut self) {
        while self.peek().is_some_and(|c| c == b' ' || c == b'\t') {
            self.i += 1;
        }
    }
    fn done(&self) -> bool {
        self.i >= self.s.len()
    }
}

/// A node reference: its id and, when it was given one here, its label and
/// shape.
struct NodeRef {
    id: String,
    label: Option<(String, Shape)>,
}

fn is_id_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c >= 0x80 || c == b'.'
}

/// One line: a node, a chain of edges, or a fan (`A & B --> C`).
fn statement(g: &mut Graph, line: &str, group: Option<usize>) -> Result<(), String> {
    let mut c = Cursor {
        s: line.as_bytes(),
        src: line,
        i: 0,
    };
    let mut prev: Vec<usize> = node_group(g, &mut c, group)?;
    loop {
        c.skip_ws();
        if c.done() {
            return Ok(());
        }
        let Some(link) = edge_op(&mut c)? else {
            return Err(format!("expected an edge at `{}`", c.rest()));
        };
        c.skip_ws();
        let next = node_group(g, &mut c, group)?;
        for &a in &prev {
            for &b in &next {
                g.edge(a, b, link.label.clone(), link.stroke);
                let e = g.edges.last_mut().expect("just pushed");
                e.from_mark = link.from_mark;
                e.to_mark = link.to_mark;
            }
        }
        prev = next;
    }
}

fn node_group(g: &mut Graph, c: &mut Cursor, group: Option<usize>) -> Result<Vec<usize>, String> {
    let mut out = Vec::new();
    loop {
        c.skip_ws();
        let r = node_ref(c)?;
        let idx = g.node(&r.id);
        if let Some((label, shape)) = r.label {
            g.nodes[idx].rows = rows(&label);
            g.nodes[idx].shape = shape;
        }
        g.assign(idx, group);
        out.push(idx);
        c.skip_ws();
        if c.peek() == Some(b'&') {
            c.i += 1;
            continue;
        }
        return Ok(out);
    }
}

fn node_ref(c: &mut Cursor) -> Result<NodeRef, String> {
    let start = c.i;
    while c.peek().is_some_and(is_id_byte) {
        // An id may hold `-` too, but never where an edge starts.
        c.i += 1;
        while c.peek() == Some(b'-')
            && c.at(1)
                .is_some_and(|n| n.is_ascii_alphanumeric() || n == b'_')
        {
            c.i += 1;
        }
    }
    if c.i == start {
        return Err(format!("expected a node at `{}`", c.rest()));
    }
    let id = c.src[start..c.i].to_string();
    // `A@{ shape: …, label: "…" }`: the newer shape syntax; keep the label.
    if c.rest().starts_with("@{") {
        let end = c.rest().find('}').ok_or("unclosed `@{`")?;
        let body = &c.rest()[2..end];
        c.i += end + 1;
        let label = body
            .split(',')
            .find_map(|kv| kv.trim().strip_prefix("label:").map(|v| clean(v.trim())));
        return Ok(NodeRef {
            id,
            label: label.map(|l| (l, Shape::Rect)),
        });
    }
    let shapes: &[(&str, &str, Shape)] = &[
        ("(((", ")))", Shape::Round),
        ("((", "))", Shape::Round),
        ("([", "])", Shape::Rect),
        ("[[", "]]", Shape::Rect),
        ("[(", ")]", Shape::Rect),
        ("[/", "/]", Shape::Rect),
        ("[/", "\\]", Shape::Rect),
        ("[\\", "\\]", Shape::Rect),
        ("[\\", "/]", Shape::Rect),
        ("{{", "}}", Shape::Diamond),
        ("[", "]", Shape::Rect),
        ("(", ")", Shape::Rect),
        ("{", "}", Shape::Diamond),
        (">", "]", Shape::Rect),
    ];
    for (open, close, shape) in shapes {
        if !c.rest().starts_with(open) {
            continue;
        }
        let body_start = c.i + open.len();
        let body = &c.src[body_start..];
        // A quoted label may hold the closing bracket.
        let end = if body.trim_start().starts_with('"') {
            let q0 = body.find('"').unwrap_or(0);
            match body[q0 + 1..].find('"') {
                Some(q1) => body[q0 + 1 + q1 + 1..]
                    .find(close)
                    .map(|k| q0 + 1 + q1 + 1 + k),
                None => None,
            }
        } else {
            body.find(close)
        };
        let Some(end) = end else { continue };
        let label = clean(&body[..end]);
        c.i = body_start + end + close.len();
        // `:::class` after a node styles it; dropped.
        if c.rest().starts_with(":::") {
            c.i += 3;
            while c.peek().is_some_and(is_id_byte) {
                c.i += 1;
            }
        }
        return Ok(NodeRef {
            id,
            label: Some((label, *shape)),
        });
    }
    if c.rest().starts_with(":::") {
        c.i += 3;
        while c.peek().is_some_and(is_id_byte) {
            c.i += 1;
        }
    }
    Ok(NodeRef { id, label: None })
}

#[derive(Debug, Clone)]
struct Link {
    label: Option<String>,
    stroke: Stroke,
    from_mark: Mark,
    to_mark: Mark,
}

fn head(c: u8) -> Option<Mark> {
    match c {
        b'>' => Some(Mark::Arrow),
        b'x' => Some(Mark::Cross),
        b'o' => Some(Mark::Circle),
        _ => None,
    }
}

/// An edge operator, with its label in either form, or `None` when the
/// cursor is not at one.
fn edge_op(c: &mut Cursor) -> Result<Option<Link>, String> {
    let save = c.i;
    let mut from_mark = Mark::None;
    match c.peek() {
        Some(b'<') => {
            from_mark = Mark::Arrow;
            c.i += 1;
        }
        Some(b'x' | b'o')
            if matches!(c.at(1), Some(b'-' | b'=' | b'.'))
                && matches!(c.at(2), Some(b'-' | b'=' | b'.')) =>
        {
            from_mark = head(c.peek().unwrap_or(b'>')).unwrap_or(Mark::None);
            c.i += 1;
        }
        _ => {}
    }
    let body_start = c.i;
    while matches!(c.peek(), Some(b'-' | b'=' | b'.')) {
        c.i += 1;
    }
    let body = &c.src[body_start..c.i];
    if body.len() < 2 && !(body == "-" && c.peek() == Some(b'.')) {
        c.i = save;
        return Ok(None);
    }
    let stroke = stroke_of(body);
    // The head, when the next byte is one and is not the start of a word.
    let mut to_mark = Mark::None;
    if let Some(m) = c.peek().and_then(head) {
        let after = c.at(1);
        let word = matches!(c.peek(), Some(b'x' | b'o'))
            && after.is_some_and(|a| a.is_ascii_alphanumeric() || a == b'_');
        if !word {
            to_mark = m;
            c.i += 1;
        }
    }
    // `-- text -->`: an opener with no head, then text, then the real op.
    let opener = to_mark == Mark::None
        && body.len() == 2
        && c.peek().is_some_and(|b| b == b' ' || b == b'\t')
        && from_mark == Mark::None;
    if opener {
        let rest = c.rest();
        let closers = ["-->", "--x", "--o", "---", "==>", "===", ".->", "-.-", ".-"];
        let mut best: Option<(usize, &str)> = None;
        for cl in closers {
            if let Some(k) = find_closer(rest, cl)
                && best.is_none_or(|(b, _)| k < b)
            {
                best = Some((k, cl));
            }
        }
        if let Some((k, _)) = best {
            let text = rest[..k].trim().to_string();
            c.i += k;
            // Consume the closing operator.
            let close_start = c.i;
            while matches!(c.peek(), Some(b'-' | b'=' | b'.')) {
                c.i += 1;
            }
            let close = &c.src[close_start..c.i];
            let stroke = stroke_of(&format!("{body}{close}"));
            let mut to_mark = Mark::None;
            if let Some(m) = c.peek().and_then(head) {
                to_mark = m;
                c.i += 1;
            }
            return Ok(Some(Link {
                label: Some(clean(&text)),
                stroke,
                from_mark,
                to_mark,
            }));
        }
    }
    let mut label = None;
    c.skip_ws();
    if c.peek() == Some(b'|') {
        let rest = &c.rest()[1..];
        let end = rest.find('|').ok_or("unclosed `|label|`")?;
        label = Some(clean(&rest[..end]));
        c.i += end + 2;
    }
    Ok(Some(Link {
        label,
        stroke,
        from_mark,
        to_mark,
    }))
}

/// Where a closing operator starts in `rest`: at a word boundary, so the
/// text `step-by-step` is not taken for an edge.
fn find_closer(rest: &str, cl: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(k) = rest[from..].find(cl) {
        let at = from + k;
        let before = rest[..at].chars().last();
        if at > 0 && before.is_some_and(|b| b == ' ' || b == '-' || b == '=' || b == '.') {
            return Some(at);
        }
        from = at + 1;
    }
    None
}

fn stroke_of(body: &str) -> Stroke {
    if body.contains('=') {
        Stroke::Thick
    } else if body.contains('.') {
        Stroke::Dotted
    } else {
        Stroke::Solid
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edges(g: &Graph) -> Vec<(String, String, Option<String>, Stroke, Mark)> {
        g.edges
            .iter()
            .map(|e| {
                (
                    g.nodes[e.from].id.clone(),
                    g.nodes[e.to].id.clone(),
                    e.label.clone(),
                    e.stroke,
                    e.to_mark,
                )
            })
            .collect()
    }

    fn e(
        a: &str,
        b: &str,
        l: Option<&str>,
        s: Stroke,
        m: Mark,
    ) -> (String, String, Option<String>, Stroke, Mark) {
        (a.into(), b.into(), l.map(String::from), s, m)
    }

    #[test]
    fn the_header_sets_the_direction() {
        assert_eq!(parse("flowchart LR\nA-->B").unwrap().dir, Dir::Right);
        assert_eq!(parse("graph TB;\nA-->B").unwrap().dir, Dir::Down);
        assert_eq!(parse("graph BT\nA-->B").unwrap().dir, Dir::Up);
        assert_eq!(parse("flowchart RL\nA-->B").unwrap().dir, Dir::Left);
        assert_eq!(parse("flowchart\nA-->B").unwrap().dir, Dir::Down);
        assert!(parse("flowchart XY\nA-->B").is_err());
    }

    #[test]
    fn every_node_shape_reads_its_label() {
        let g = parse(
            "flowchart TD\n a[rect] --> b(round) --> c([stadium]) --> d[[sub]]\n\
             d --> e{decide} --> f((circle)) --> h>flag] --> i[(db)] --> j{{hex}}",
        )
        .unwrap();
        let shape = |id: &str| g.nodes[g.find(id).unwrap()].shape;
        let label = |id: &str| g.nodes[g.find(id).unwrap()].rows.clone();
        assert_eq!(label("a"), vec![Row::Text("rect".into())]);
        assert_eq!(label("c"), vec![Row::Text("stadium".into())]);
        assert_eq!(label("h"), vec![Row::Text("flag".into())]);
        assert_eq!(label("i"), vec![Row::Text("db".into())]);
        assert_eq!(shape("e"), Shape::Diamond);
        assert_eq!(shape("j"), Shape::Diamond);
        assert_eq!(shape("f"), Shape::Round);
        assert_eq!(shape("b"), Shape::Rect);
        assert_eq!(g.edges.len(), 3 + 5);
    }

    #[test]
    fn edges_in_all_their_forms() {
        let g = parse(
            "flowchart LR\n\
             A --> B\n A --- C\n A -.-> D\n A ==> E\n A --x F\n A --o G\n\
             A -->|yes| H\n A -- no --> I\n A -. maybe .-> J\n A == sure ==> K\n A ---->L",
        )
        .unwrap();
        use Mark::{Arrow, Circle, Cross};
        use Stroke::*;
        assert_eq!(
            edges(&g),
            vec![
                e("A", "B", None, Solid, Arrow),
                e("A", "C", None, Solid, Mark::None),
                e("A", "D", None, Dotted, Arrow),
                e("A", "E", None, Thick, Arrow),
                e("A", "F", None, Solid, Cross),
                e("A", "G", None, Solid, Circle),
                e("A", "H", Some("yes"), Solid, Arrow),
                e("A", "I", Some("no"), Solid, Arrow),
                e("A", "J", Some("maybe"), Dotted, Arrow),
                e("A", "K", Some("sure"), Thick, Arrow),
                e("A", "L", None, Solid, Arrow),
            ]
        );
    }

    #[test]
    fn chains_fans_and_two_way_arrows() {
        let g = parse("graph TD\n A --> B --> C\n A & B --> D & E\n X <--> Y").unwrap();
        assert_eq!(g.edges.len(), 2 + 4 + 1);
        let two_way = g.edges.last().unwrap();
        assert_eq!(
            (two_way.from_mark, two_way.to_mark),
            (Mark::Arrow, Mark::Arrow)
        );
    }

    #[test]
    fn subgraphs_nest_and_take_their_nodes() {
        let g = parse(
            "flowchart TD\n a --> b\n subgraph outer [The outside]\n  b --> c\n  subgraph inner\n   c --> d\n  end\n end\n %% a comment\n d --> e",
        )
        .unwrap();
        assert_eq!(g.groups.len(), 2);
        assert_eq!(g.groups[0].title, "The outside");
        assert_eq!(g.groups[1].title, "inner");
        assert_eq!(g.groups[1].parent, Some(0));
        let group = |id: &str| g.nodes[g.find(id).unwrap()].group;
        assert_eq!(group("a"), None);
        assert_eq!(group("b"), Some(0));
        assert_eq!(group("c"), Some(1), "a nested subgraph takes a node deeper");
        assert_eq!(group("d"), Some(1));
        assert_eq!(group("e"), None);
    }

    #[test]
    fn styling_lines_are_dropped_and_labels_cleaned() {
        let g = parse(
            "flowchart LR\n classDef hot fill:#f00\n A[\"quoted [brackets]\"]:::hot --> B[line one<br/>line two]\n style A fill:#fff\n class B hot\n linkStyle 0 stroke:#f00\n click A callback",
        )
        .unwrap();
        assert_eq!(g.nodes[0].rows, vec![Row::Text("quoted [brackets]".into())]);
        assert_eq!(
            g.nodes[1].rows,
            vec![Row::Text("line one".into()), Row::Text("line two".into())]
        );
    }

    #[test]
    fn ids_with_dashes_and_semicolons_between_statements() {
        let g = parse("graph LR\n tour-walk --> file-tabs; file-tabs --> ui").unwrap();
        assert_eq!(g.nodes.len(), 3);
        assert_eq!(g.nodes[0].id, "tour-walk");
    }

    #[test]
    fn a_line_that_is_not_a_statement_names_itself() {
        let err = parse("flowchart TD\n A --> B\n A --> \n").unwrap_err();
        assert!(err.starts_with("line 3"), "{err}");
        assert!(parse("flowchart TD\n end").is_err());
        assert!(parse("flowchart TD\n A -->|open B").is_err());
    }
}
