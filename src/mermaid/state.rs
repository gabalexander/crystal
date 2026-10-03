//! `stateDiagram-v2` read into a [`Graph`] and drawn by the layered
//! engine, the way a state machine reads best: states as boxes,
//! transitions as labelled arrows, `[*]` as `●` where the machine starts
//! and `◉` where it ends.
//!
//! Every composite state (`state Review { … }`) is a frame with its own
//! `[*]`s: a start inside a composite is a different node from the one
//! outside it, exactly as in mermaid. A transition into a composite enters
//! it at its start (or its first state), and one out of it leaves from its
//! end (or its last state): the engine joins nodes, not frames, and that is
//! what the frame's edge means anyway. `<<choice>>` is a diamond,
//! `<<fork>>` and `<<join>>` a bar; notes, `--` concurrency separators and
//! styling are read and dropped.

use super::flowchart::clean;
use super::graph::{Dir, Graph, Row, Shape};

/// A transition as read, resolved once every composite is known.
struct Pending {
    from: String,
    to: String,
    label: Option<String>,
}

pub fn parse(src: &str) -> Result<Graph, String> {
    let mut lines = super::lines(src);
    lines.next();
    let mut g = Graph::new(Dir::Down);
    // Composite states by name: their group and their scope key.
    let mut composites: Vec<(String, usize)> = Vec::new();
    let mut stack: Vec<(String, usize)> = Vec::new();
    let mut pending: Vec<Pending> = Vec::new();
    let mut in_note = false;
    for (no, line) in lines {
        if in_note {
            if line.starts_with("end note") {
                in_note = false;
            }
            continue;
        }
        let scope = stack.last().map(|(n, _)| n.clone()).unwrap_or_default();
        let group = stack.last().map(|(_, gid)| *gid);
        let first = line.split_whitespace().next().unwrap_or("");
        match first {
            "note" => {
                if !line.contains(':') {
                    in_note = true;
                }
                continue;
            }
            "direction" => {
                if stack.is_empty()
                    && let Some(d) = line.split_whitespace().nth(1).and_then(Dir::parse)
                {
                    g.dir = d;
                }
                continue;
            }
            "}" => {
                if stack.pop().is_none() {
                    return Err(format!("line {no}: `}}` without a composite state"));
                }
                continue;
            }
            "--" | "classDef" | "class" | "style" | "scale" | "hide" | "accTitle" | "accDescr"
            | "title" => continue,
            "state" => {
                let rest = line["state".len()..].trim();
                if let Some(body) = rest.strip_suffix('{') {
                    let name = body.trim();
                    let name = strip_class(name);
                    let (id, title) = match name.split_once(" as ") {
                        Some((a, b)) if a.trim().starts_with('"') => (b.trim(), clean(a)),
                        Some((a, b)) => (a.trim(), clean(b)),
                        None => (name, name.to_string()),
                    };
                    let gid = g.add_group(&title, group);
                    composites.push((id.to_string(), gid));
                    stack.push((id.to_string(), gid));
                    continue;
                }
                if let Some((desc, id)) = rest.split_once(" as ") {
                    let (desc, id) = if desc.trim().starts_with('"') {
                        (desc, id)
                    } else {
                        (id, desc)
                    };
                    let id = strip_class(id.trim());
                    let n = g.node(id);
                    g.nodes[n].rows = super::flowchart::rows(&clean(desc));
                    g.assign(n, group);
                    continue;
                }
                let mut words = rest.split_whitespace();
                let id = strip_class(words.next().unwrap_or(""));
                if id.is_empty() {
                    return Err(format!("line {no}: `state` without a name"));
                }
                let n = g.node(id);
                g.assign(n, group);
                match words.next() {
                    Some("<<choice>>") => {
                        g.nodes[n].shape = Shape::Diamond;
                        g.nodes[n].rows = vec![Row::Text(String::new())];
                    }
                    Some("<<fork>>") | Some("<<join>>") => g.nodes[n].shape = Shape::Bar,
                    _ => {}
                }
                continue;
            }
            _ => {}
        }
        if let Some((left, right)) = line.split_once("-->") {
            let (to, label) = match right.split_once(':') {
                Some((t, l)) => (t.trim(), Some(clean(l))),
                None => (right.trim(), None),
            };
            let from = endpoint(&mut g, strip_class(left.trim()), &scope, group, true)?;
            let to = endpoint(&mut g, strip_class(to), &scope, group, false)?;
            pending.push(Pending { from, to, label });
            continue;
        }
        if let Some((id, desc)) = line.split_once(':') {
            let id = strip_class(id.trim());
            if id.is_empty() || id.contains(char::is_whitespace) {
                return Err(format!("line {no}: cannot read `{line}`"));
            }
            let n = g.node(id);
            g.assign(n, group);
            let rows = &mut g.nodes[n].rows;
            if !rows.contains(&Row::Rule) {
                rows.push(Row::Rule);
            }
            rows.push(Row::Text(clean(desc)));
            continue;
        }
        let id = strip_class(line);
        if id.is_empty() || id.contains(char::is_whitespace) {
            return Err(format!("line {no}: cannot read `{line}`"));
        }
        let n = g.node(id);
        g.assign(n, group);
    }
    if !stack.is_empty() {
        return Err("a composite state is never closed with `}`".into());
    }
    // Transitions: a composite's name stands for its start going in and its
    // end coming out.
    let inside = |g: &Graph, gid: usize, start: bool| -> Option<usize> {
        let key = format!(
            "[*]{}:{}",
            if start { "start" } else { "end" },
            composite_key(gid, &composites)
        );
        if let Some(n) = g.find(&key) {
            return Some(n);
        }
        let members: Vec<usize> = (0..g.nodes.len())
            .filter(|&n| g.nodes[n].group == Some(gid))
            .collect();
        if start {
            members.first().copied()
        } else {
            members.last().copied()
        }
    };
    for p in pending {
        let resolve = |g: &mut Graph, name: &str, start: bool| -> usize {
            if let Some((_, gid)) = composites.iter().find(|(n, _)| n == name)
                && let Some(n) = inside(g, *gid, start)
            {
                return n;
            }
            g.node(name)
        };
        let from = resolve(&mut g, &p.from, false);
        let to = resolve(&mut g, &p.to, true);
        g.edge(from, to, p.label, super::canvas::Stroke::Solid);
    }
    // A composite named in a transition was made a box when it was read;
    // its transitions now join its inside, so the box goes.
    let orphans: Vec<usize> = composites
        .iter()
        .filter_map(|(name, _)| g.find(name))
        .filter(|&n| !g.edges.iter().any(|e| e.from == n || e.to == n))
        .collect();
    if !orphans.is_empty() {
        g = without(g, &orphans);
    }
    Ok(g)
}

/// The scope a composite's `[*]` nodes are keyed by: its name.
fn composite_key(gid: usize, composites: &[(String, usize)]) -> String {
    composites
        .iter()
        .find(|(_, x)| *x == gid)
        .map(|(n, _)| n.clone())
        .unwrap_or_default()
}

/// `S:::cls` → `S`.
fn strip_class(s: &str) -> &str {
    s.split(":::").next().unwrap_or(s).trim()
}

/// An endpoint of a transition: a state's name, or `[*]` made into this
/// scope's start (on the left of an arrow) or end (on the right) node.
fn endpoint(
    g: &mut Graph,
    name: &str,
    scope: &str,
    group: Option<usize>,
    source: bool,
) -> Result<String, String> {
    if name.is_empty() {
        return Err("a transition is missing a state".into());
    }
    if name.contains(char::is_whitespace) {
        return Err(format!("cannot read the state `{name}`"));
    }
    if name == "[*]" {
        let key = format!("[*]{}:{scope}", if source { "start" } else { "end" });
        let n = g.node(&key);
        g.nodes[n].shape = if source { Shape::Start } else { Shape::End };
        g.nodes[n].rows = Vec::new();
        g.assign(n, group);
        return Ok(key);
    }
    let n = g.node(name);
    g.assign(n, group);
    Ok(name.to_string())
}

/// `g` with the nodes in `drop` removed (they have no edges).
fn without(g: Graph, drop: &[usize]) -> Graph {
    let mut out = Graph::new(g.dir);
    out.groups = g.groups.clone();
    let mut map = vec![usize::MAX; g.nodes.len()];
    for (i, node) in g.nodes.iter().enumerate() {
        if drop.contains(&i) {
            continue;
        }
        let n = out.node(&node.id);
        out.nodes[n] = node.clone();
        map[i] = n;
    }
    for e in &g.edges {
        let mut e = e.clone();
        e.from = map[e.from];
        e.to = map[e.to];
        out.edges.push(e);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_and_end_are_their_own_shapes_and_labels_follow_the_colon() {
        let g =
            parse("stateDiagram-v2\n [*] --> Idle\n Idle --> Busy : go\n Busy --> [*]").unwrap();
        let start = g.find("[*]start:").unwrap();
        let end = g.find("[*]end:").unwrap();
        assert_eq!(g.nodes[start].shape, Shape::Start);
        assert_eq!(g.nodes[end].shape, Shape::End);
        assert_eq!(g.edges.len(), 3);
        assert_eq!(g.edges[1].label.as_deref(), Some("go"));
    }

    #[test]
    fn described_states_and_descriptions_after_a_colon() {
        let g = parse(
            "stateDiagram-v2\n state \"Waiting for review\" as W\n S : first line\n S : second\n W --> S",
        )
        .unwrap();
        let w = g.find("W").unwrap();
        assert_eq!(
            g.nodes[w].rows,
            vec![Row::Text("Waiting for review".into())]
        );
        let s = g.find("S").unwrap();
        assert_eq!(
            g.nodes[s].rows,
            vec![
                Row::Text("S".into()),
                Row::Rule,
                Row::Text("first line".into()),
                Row::Text("second".into())
            ]
        );
    }

    #[test]
    fn composites_are_frames_with_their_own_start_and_end() {
        let g = parse(
            "stateDiagram-v2\n [*] --> A\n A --> C\n state C {\n  [*] --> X\n  X --> [*]\n }\n C --> [*]",
        )
        .unwrap();
        assert_eq!(g.groups.len(), 1);
        let inner_start = g.find("[*]start:C").unwrap();
        let inner_end = g.find("[*]end:C").unwrap();
        assert_eq!(g.nodes[inner_start].group, Some(0));
        assert_eq!(g.nodes[g.find("X").unwrap()].group, Some(0));
        assert_ne!(inner_start, g.find("[*]start:").unwrap());
        let a = g.find("A").unwrap();
        assert!(
            g.edges.iter().any(|e| e.from == a && e.to == inner_start),
            "into C is into its start"
        );
        let outer_end = g.find("[*]end:").unwrap();
        assert!(
            g.edges
                .iter()
                .any(|e| e.from == inner_end && e.to == outer_end),
            "out of C is out of its end"
        );
        assert!(g.find("C").is_none(), "the composite is a frame, not a box");
    }

    #[test]
    fn choice_fork_join_direction_and_notes() {
        let g = parse(
            "stateDiagram-v2\n direction LR\n state c <<choice>>\n state f <<fork>>\n note right of c\n  a note\n end note\n note left of f : short\n c --> f",
        )
        .unwrap();
        assert_eq!(g.dir, Dir::Right);
        assert_eq!(g.nodes[g.find("c").unwrap()].shape, Shape::Diamond);
        assert_eq!(g.nodes[g.find("f").unwrap()].shape, Shape::Bar);
        assert_eq!(g.nodes.len(), 2);
    }

    #[test]
    fn unreadable_lines_and_unbalanced_braces_are_errors() {
        assert!(parse("stateDiagram-v2\n A B C").is_err());
        assert!(parse("stateDiagram-v2\n }").is_err());
        assert!(parse("stateDiagram-v2\n state X {\n A --> B").is_err());
        assert!(parse("stateDiagram-v2\n --> B").is_err());
    }
}
