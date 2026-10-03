//! `sequenceDiagram` read and drawn in a layout of its own, the one a
//! request path reads best in: the participants as boxes across
//! the top, a lifeline down from each, one row per message with the arrow
//! between the two lifelines and its words above it.
//!
//! ```text
//! ┌────────┐     ┌────────┐
//! │ client │     │ server │
//! └───┬────┘     └───┬────┘
//!     │     poll     │
//!     ├─────────────▶│
//! ```
//!
//! The six message arrows keep their meaning in the glyph at the end (`▶`
//! a call, `▷` an async message, `×` a lost one, a bare line joining the
//! lifeline for the open forms) and their dashes in the line (dotted for a
//! reply). A self-message is a small loop to the right of its
//! lifeline. Notes are boxes over, left of or right of a lifeline; `loop`,
//! `alt`, `opt`, `par`, `critical` and `break` are labelled frames across
//! the diagram, with `else`/`and`/`option` as dotted dividers; an active
//! participant's lifeline is drawn thick; `autonumber` numbers the
//! messages.
//!
//! When the diagram is wider than the page: the participant boxes shrink to
//! their names, then every label is cut shorter and shorter, and when even
//! that doesn't fit, with too many participants for the width, the diagram
//! is refused and the source is shown.

use super::canvas::{Canvas, Glyphs, LEFT, RIGHT, Stroke};
use super::flowchart::clean;
use super::width::{display_width, truncate};

/// Label caps tried in turn once the boxes are as small as they go.
const CAPS: &[usize] = &[40, 32, 24, 18, 14, 11, 8, 6];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Participant {
    pub id: String,
    pub name: String,
    pub actor: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Head {
    /// `->>`: a filled arrowhead.
    Arrow,
    /// `->`: no head, the line meets the lifeline.
    Open,
    /// `-x`: a cross.
    Cross,
    /// `-)`: an open (async) arrowhead.
    Async,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteAt {
    Left(usize),
    Right(usize),
    Over(usize, usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Message {
        from: usize,
        to: usize,
        text: String,
        stroke: Stroke,
        head: Head,
        /// `+`: the target is activated by it.
        activate: bool,
        /// `-`: the source is deactivated by it.
        deactivate: bool,
    },
    Note {
        at: NoteAt,
        text: String,
    },
    Activate(usize),
    Deactivate(usize),
    /// `loop`, `alt`, `opt`, `par`, `critical`, `break`.
    Open {
        kind: String,
        text: String,
    },
    /// `else`, `and`, `option`.
    Divider {
        text: String,
    },
    Close,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Diagram {
    pub participants: Vec<Participant>,
    pub events: Vec<Event>,
    pub autonumber: bool,
}

impl Diagram {
    fn participant(&mut self, id: &str) -> usize {
        if let Some(i) = self.participants.iter().position(|p| p.id == id) {
            return i;
        }
        self.participants.push(Participant {
            id: id.to_string(),
            name: id.to_string(),
            actor: false,
        });
        self.participants.len() - 1
    }
}

/// Arrows, longest first so `-->>` is not read as `-->` and a stray `>`.
const ARROWS: &[(&str, Stroke, Head)] = &[
    ("-->>", Stroke::Dotted, Head::Arrow),
    ("->>", Stroke::Solid, Head::Arrow),
    ("--x", Stroke::Dotted, Head::Cross),
    ("-x", Stroke::Solid, Head::Cross),
    ("--)", Stroke::Dotted, Head::Async),
    ("-)", Stroke::Solid, Head::Async),
    ("-->", Stroke::Dotted, Head::Open),
    ("->", Stroke::Solid, Head::Open),
];

const FRAMES: &[&str] = &["loop", "alt", "opt", "par", "critical", "break"];
const DIVIDERS: &[&str] = &["else", "and", "option"];
const IGNORED: &[&str] = &[
    "title",
    "accTitle",
    "accDescr",
    "links",
    "link",
    "properties",
    "details",
    "destroy",
];

pub fn parse(src: &str) -> Result<Diagram, String> {
    let mut lines = super::lines(src);
    lines.next();
    let mut d = Diagram::default();
    // Open blocks: `true` for a frame that draws, `false` for `rect`/`box`.
    let mut open: Vec<bool> = Vec::new();
    for (no, line) in lines {
        let err = |what: &str| format!("line {no}: {what}");
        let (first, rest) = match line.split_once(char::is_whitespace) {
            Some((f, r)) => (f, r.trim()),
            None => (line, ""),
        };
        let lower = first.to_ascii_lowercase();
        match lower.as_str() {
            "participant" | "actor" | "create" => {
                let (actor, rest) = if lower == "create" {
                    match rest.split_once(char::is_whitespace) {
                        Some((kind, r)) => (kind == "actor", r.trim()),
                        None => return Err(err("`create` names no participant")),
                    }
                } else {
                    (lower == "actor", rest)
                };
                let rest = rest.split("@{").next().unwrap_or(rest).trim();
                let (id, name) = match rest.split_once(" as ") {
                    Some((id, name)) => (id.trim(), clean(name).replace('\n', " ")),
                    None => (rest, clean(rest)),
                };
                if id.is_empty() {
                    return Err(err("a participant with no name"));
                }
                let i = d.participant(id);
                d.participants[i].name = name;
                d.participants[i].actor = actor;
                continue;
            }
            "autonumber" => {
                d.autonumber = rest != "off";
                continue;
            }
            "activate" | "deactivate" => {
                if rest.is_empty() {
                    return Err(err("activate names no participant"));
                }
                let i = d.participant(rest);
                d.events.push(if lower == "activate" {
                    Event::Activate(i)
                } else {
                    Event::Deactivate(i)
                });
                continue;
            }
            "note" => {
                let (place, text) = rest
                    .split_once(':')
                    .ok_or_else(|| err("a note needs `: text`"))?;
                let text = clean(text);
                let place = place.trim();
                let lower = place.to_ascii_lowercase();
                let at = if let Some(who) = lower.strip_prefix("left of") {
                    NoteAt::Left(d.participant(place[place.len() - who.len()..].trim()))
                } else if let Some(who) = lower.strip_prefix("right of") {
                    NoteAt::Right(d.participant(place[place.len() - who.len()..].trim()))
                } else if let Some(who) = lower.strip_prefix("over") {
                    let who = &place[place.len() - who.len()..];
                    let mut names = who.split(',').map(str::trim).filter(|s| !s.is_empty());
                    let a = names.next().ok_or_else(|| err("a note over nobody"))?;
                    let a = d.participant(a);
                    let b = match names.next() {
                        Some(b) => d.participant(b),
                        None => a,
                    };
                    NoteAt::Over(a.min(b), a.max(b))
                } else {
                    return Err(err("a note is `left of`, `right of` or `over`"));
                };
                d.events.push(Event::Note { at, text });
                continue;
            }
            "end" if rest.is_empty() => {
                match open.pop() {
                    Some(true) => d.events.push(Event::Close),
                    Some(false) => {}
                    None => return Err(err("`end` without a block")),
                }
                continue;
            }
            "rect" | "box" => {
                open.push(false);
                continue;
            }
            k if FRAMES.contains(&k) => {
                open.push(true);
                d.events.push(Event::Open {
                    kind: k.to_string(),
                    text: clean(rest).replace('\n', " "),
                });
                continue;
            }
            k if DIVIDERS.contains(&k) => {
                if open.last() != Some(&true) {
                    return Err(err("a divider outside a block"));
                }
                d.events.push(Event::Divider {
                    text: clean(rest).replace('\n', " "),
                });
                continue;
            }
            k if IGNORED.contains(&k) => continue,
            _ => {}
        }
        message(&mut d, line).ok_or_else(|| err(&format!("cannot read `{line}`")))?;
    }
    if !open.is_empty() {
        return Err("a block is never closed with `end`".into());
    }
    Ok(d)
}

/// `A->>+B: text`.
fn message(d: &mut Diagram, line: &str) -> Option<()> {
    let bytes = line.as_bytes();
    let mut found = None;
    'scan: for i in 0..bytes.len() {
        if bytes[i] != b'-' {
            continue;
        }
        for (arrow, stroke, head) in ARROWS {
            if line[i..].starts_with(arrow) {
                found = Some((i, arrow.len(), *stroke, *head));
                break 'scan;
            }
        }
    }
    let (at, len, stroke, head) = found?;
    let from = line[..at].trim();
    let rest = &line[at + len..];
    let (target, text) = match rest.split_once(':') {
        Some((t, x)) => (t, clean(x)),
        None => (rest, String::new()),
    };
    let mut target = target.trim();
    let mut activate = false;
    let mut deactivate = false;
    if let Some(t) = target.strip_prefix('+') {
        activate = true;
        target = t.trim();
    } else if let Some(t) = target.strip_prefix('-') {
        deactivate = true;
        target = t.trim();
    }
    if from.is_empty()
        || target.is_empty()
        || target.contains(char::is_whitespace) && target.contains("->")
    {
        return None;
    }
    let from = d.participant(from);
    let to = d.participant(target);
    d.events.push(Event::Message {
        from,
        to,
        text,
        stroke,
        head,
        activate,
        deactivate,
    });
    Some(())
}

/// Draw `d` in at most `max_width` columns, down the ladder: as it is,
/// boxes shrunk to their names, labels cut shorter and shorter.
pub fn draw(d: &Diagram, glyphs: &Glyphs, max_width: usize) -> Result<Vec<String>, String> {
    if d.participants.is_empty() {
        return Err("the diagram has no participants".into());
    }
    let longest = d
        .events
        .iter()
        .map(|e| match e {
            Event::Message { text, .. } | Event::Note { text, .. } => {
                text.split('\n').map(display_width).max().unwrap_or(0) + 4
            }
            _ => 0,
        })
        .chain(d.participants.iter().map(|p| display_width(&p.name)))
        .max()
        .unwrap_or(0);
    let mut attempts = vec![(false, None), (true, None)];
    attempts.extend(
        CAPS.iter()
            .filter(|c| **c < longest)
            .map(|c| (true, Some(*c))),
    );
    let mut narrowest = usize::MAX;
    for (compact, cap) in attempts {
        let lines = layout(d, glyphs, compact, cap);
        let width = lines.iter().map(|l| display_width(l)).max().unwrap_or(0);
        if width <= max_width {
            return Ok(lines);
        }
        narrowest = narrowest.min(width);
    }
    Err(format!(
        "{} participants need {narrowest} columns even with the boxes shrunk and the labels cut short; {max_width} available",
        d.participants.len()
    ))
}

/// A note's box, as placed: columns, rows and text.
struct Placed {
    x: usize,
    y: usize,
    w: usize,
    lines: Vec<String>,
}

fn layout(d: &Diagram, glyphs: &Glyphs, compact: bool, cap: Option<usize>) -> Vec<String> {
    let cut = |s: &str| -> String {
        let t = glyphs.text(s.to_string());
        glyphs.text(match cap {
            Some(c) => truncate(&t, c),
            None => t,
        })
    };
    let n = d.participants.len();
    let names: Vec<String> = d
        .participants
        .iter()
        .map(|p| match cap {
            Some(c) => cut_to(&p.name, c.max(6), glyphs),
            None => p.name.clone(),
        })
        .collect();
    let bw: Vec<usize> = names
        .iter()
        .map(|nm| display_width(nm) + if compact { 2 } else { 4 })
        .collect();
    let gap = if compact { 1 } else { 2 };

    // Every event's text, cut and numbered.
    let mut number = 0;
    let texts: Vec<Vec<String>> = d
        .events
        .iter()
        .map(|e| match e {
            Event::Message { text, .. } => {
                number += 1;
                let mut lines: Vec<String> =
                    text.split('\n').map(|l| l.trim().to_string()).collect();
                if d.autonumber {
                    lines[0] = format!("{number}. {}", lines[0]).trim_end().to_string();
                }
                lines.iter().map(|l| cut(l)).collect()
            }
            Event::Note { text, .. } => text.split('\n').map(|l| cut(l.trim())).collect(),
            Event::Open { text, .. } | Event::Divider { text } => vec![text.clone()],
            _ => Vec::new(),
        })
        .collect();
    let widest = |ls: &[String]| ls.iter().map(|l| display_width(l)).max().unwrap_or(0);

    // ---- columns ----
    let mut dist: Vec<usize> = (0..n.saturating_sub(1))
        .map(|i| (bw[i] - 1 - bw[i] / 2) + bw[i + 1] / 2 + 1 + gap)
        .collect();
    let mut spans: Vec<(usize, usize, usize)> = Vec::new();
    let mut left_need = 0usize;
    let mut right_need = 0usize;
    for (e, t) in d.events.iter().zip(&texts) {
        let w = widest(t);
        match e {
            Event::Message { from, to, .. } if from != to => {
                spans.push(((*from).min(*to), (*from).max(*to), w + 4));
            }
            Event::Message { from, .. } => {
                if *from + 1 < n {
                    spans.push((*from, *from + 1, w + 6));
                } else {
                    right_need = right_need.max(w + 5);
                }
            }
            Event::Note { at, .. } => {
                let nw = w + 4;
                match *at {
                    NoteAt::Right(a) => {
                        if a + 1 < n {
                            spans.push((a, a + 1, nw + 3));
                        } else {
                            right_need = right_need.max(nw + 2);
                        }
                    }
                    NoteAt::Left(a) => {
                        if a > 0 {
                            spans.push((a - 1, a, nw + 3));
                        } else {
                            left_need = left_need.max(nw + 2);
                        }
                    }
                    NoteAt::Over(a, b) if a == b => {
                        let (l, r) = (nw / 2 + 2, nw - 1 - nw / 2 + 2);
                        if a > 0 {
                            spans.push((a - 1, a, l));
                        } else {
                            left_need = left_need.max(l - 1);
                        }
                        if a + 1 < n {
                            spans.push((a, a + 1, r));
                        } else {
                            right_need = right_need.max(r - 1);
                        }
                    }
                    NoteAt::Over(a, b) => {
                        spans.push((a, b, nw.saturating_sub(5)));
                        if a == 0 {
                            left_need = left_need.max(2);
                        }
                        if b + 1 == n {
                            right_need = right_need.max(2);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    spans.sort_by_key(|&(a, b, _)| b - a);
    for (a, b, need) in spans {
        let have: usize = dist[a..b].iter().sum();
        if have < need {
            let extra = need - have;
            let k = b - a;
            for (j, dj) in dist[a..b].iter_mut().enumerate() {
                *dj += extra / k + usize::from(j < extra % k);
            }
        }
    }
    let depth = {
        let (mut cur, mut max) = (0usize, 0usize);
        for e in &d.events {
            match e {
                Event::Open { .. } => {
                    cur += 1;
                    max = max.max(cur);
                }
                Event::Close => cur = cur.saturating_sub(1),
                _ => {}
            }
        }
        max
    };
    let inset = if depth > 0 { depth + 1 } else { 0 };
    let x0 = (bw[0] / 2).max(inset + left_need);
    let mut xs = vec![x0];
    for dd in &dist {
        let last = *xs.last().unwrap_or(&0);
        xs.push(last + dd);
    }
    let xl = xs[n - 1];
    let width = xl + (bw[n - 1] - 1 - bw[n - 1] / 2).max(right_need + inset) + 1;

    // ---- rows ----
    let mut cv = Canvas::new();
    let mut y = 3;
    let mut active: Vec<Vec<(usize, Option<usize>)>> = vec![Vec::new(); n];
    let mut notes: Vec<Placed> = Vec::new();
    let mut frames: Vec<(usize, usize, String)> = Vec::new(); // (depth, top row, label)
    let mut closed: Vec<(usize, usize, usize, String)> = Vec::new(); // (depth, top, bottom, label)
    let mut dividers: Vec<(usize, usize, String)> = Vec::new();
    let mut messages: Vec<(usize, usize, usize, Vec<String>, Stroke, Head)> = Vec::new(); // (row, from, to, text, …)
    let start = |active: &mut Vec<Vec<(usize, Option<usize>)>>, p: usize, row: usize| {
        active[p].push((row, None));
    };
    let stop = |active: &mut Vec<Vec<(usize, Option<usize>)>>, p: usize, row: usize| {
        if let Some(open) = active[p].iter_mut().rev().find(|(_, end)| end.is_none()) {
            open.1 = Some(row);
        }
    };
    for (e, t) in d.events.iter().zip(&texts) {
        match e {
            Event::Message {
                from,
                to,
                stroke,
                head,
                activate,
                deactivate,
                ..
            } => {
                let lines = t.len().max(1);
                messages.push((y, *from, *to, t.clone(), *stroke, *head));
                let arrow = y + lines;
                if *activate {
                    start(&mut active, *to, arrow);
                }
                if *deactivate {
                    stop(&mut active, *from, arrow + 1);
                }
                y = arrow + 1;
            }
            Event::Note { at, .. } => {
                let nw = widest(t) + 4;
                let (x, w) = match *at {
                    NoteAt::Right(a) => (xs[a] + 2, nw),
                    NoteAt::Left(a) => (xs[a].saturating_sub(1 + nw), nw),
                    NoteAt::Over(a, b) if a == b => (xs[a].saturating_sub(nw / 2), nw),
                    NoteAt::Over(a, b) => {
                        let w = (xs[b] - xs[a] + 5).max(nw);
                        (xs[a].saturating_sub(2), w)
                    }
                };
                notes.push(Placed {
                    x,
                    y,
                    w,
                    lines: t.clone(),
                });
                y += t.len() + 2;
            }
            Event::Activate(p) => start(&mut active, *p, y),
            Event::Deactivate(p) => stop(&mut active, *p, y),
            Event::Open { kind, text } => {
                let label = if text.is_empty() {
                    kind.clone()
                } else {
                    format!("{kind} [{text}]")
                };
                frames.push((frames.len(), y, label));
                y += 1;
            }
            Event::Divider { text } => {
                let k = frames.len().saturating_sub(1);
                dividers.push((
                    k,
                    y,
                    if text.is_empty() {
                        String::new()
                    } else {
                        format!("[{text}]")
                    },
                ));
                y += 1;
            }
            Event::Close => {
                if let Some((k, top, label)) = frames.pop() {
                    closed.push((k, top, y, label));
                }
                y += 1;
            }
        }
    }
    let last = y;

    // ---- drawing ----
    // Lifelines, broken where a note covers them so its border joins them.
    for (p, &x) in xs.iter().enumerate() {
        let mut covered: Vec<(usize, usize)> = notes
            .iter()
            .filter(|nt| x >= nt.x && x < nt.x + nt.w)
            .map(|nt| (nt.y, nt.y + nt.lines.len() + 1))
            .collect();
        covered.sort();
        let mut from = 2;
        for (top, bottom) in covered {
            if top > from {
                cv.vline(x, from, top, Stroke::Solid);
            }
            from = bottom;
        }
        if last > from {
            cv.vline(x, from, last, Stroke::Solid);
        }
        for &(a, b) in &active[p] {
            let b = b.unwrap_or(last);
            if b > a + 1 {
                // Thick only where the lifeline is drawn: `join` keeps the
                // sides and changes the stroke.
                for row in a..b {
                    if cv.occupied(x, row) {
                        cv.join(x, row, 0, Stroke::Thick);
                    }
                }
            }
        }
    }
    // Participants.
    for (p, &x) in xs.iter().enumerate() {
        let left = x - bw[p] / 2;
        cv.clear(left + 1, 1, bw[p] - 2, 1);
        cv.rect(left, 0, bw[p], 3, Stroke::Solid, d.participants[p].actor);
        let tw = display_width(&names[p]);
        cv.text(left + 1 + (bw[p] - 2 - tw) / 2, 1, &names[p]);
    }
    // Frames and dividers.
    for (k, top, bottom, label) in &closed {
        let (l, r) = (*k, width - 1 - k);
        cv.rect(l, *top, r - l + 1, bottom - top + 1, Stroke::Solid, false);
        let room = (r - l + 1).saturating_sub(6);
        cv.text(l + 2, *top, &format!(" {} ", cut_to(label, room, glyphs)));
    }
    for (k, row, label) in &dividers {
        let (l, r) = (*k, width - 1 - k);
        cv.hline(*row, l, r, Stroke::Dotted);
        if !label.is_empty() {
            let room = (r - l + 1).saturating_sub(6);
            cv.text(l + 2, *row, &format!(" {} ", cut_to(label, room, glyphs)));
        }
    }
    // Messages.
    for (row, from, to, text, stroke, head) in &messages {
        let arrow = row + text.len().max(1);
        let (a, b) = (xs[*from], xs[*to]);
        if from == to {
            cv.hline(*row, a, a + 2, *stroke);
            cv.vline(a + 2, *row, arrow, *stroke);
            cv.hline(arrow, a, a + 2, *stroke);
            if let Some(c) = head_glyph(*head, LEFT, glyphs) {
                cv.put(a + 1, arrow, c);
            }
            for (i, t) in text.iter().enumerate() {
                cv.text(a + 4, row + i, t);
            }
            continue;
        }
        let right = b > a;
        let tip = if right { b - 1 } else { b + 1 };
        match head_glyph(*head, if right { RIGHT } else { LEFT }, glyphs) {
            Some(c) => {
                cv.hline(arrow, a, tip, *stroke);
                cv.put(tip, arrow, c);
            }
            None => cv.hline(arrow, a, b, *stroke),
        }
        let (lo, hi) = (a.min(b), a.max(b));
        let room = hi - lo - 1;
        for (i, t) in text.iter().enumerate() {
            let tw = display_width(t);
            cv.text(lo + 1 + room.saturating_sub(tw) / 2, row + i, t);
        }
    }
    // Notes on top.
    for nt in &notes {
        let h = nt.lines.len() + 2;
        cv.clear(nt.x + 1, nt.y + 1, nt.w - 2, h - 2);
        cv.rect(nt.x, nt.y, nt.w, h, Stroke::Solid, false);
        for (i, t) in nt.lines.iter().enumerate() {
            cv.text(nt.x + 2, nt.y + 1 + i, t);
        }
    }
    cv.lines(glyphs)
}

/// `s` cut to `max` columns with the glyph set's ellipsis.
fn cut_to(s: &str, max: usize, glyphs: &Glyphs) -> String {
    glyphs.text(truncate(&glyphs.text(s.to_string()), max))
}

fn head_glyph(head: Head, side: u8, glyphs: &Glyphs) -> Option<char> {
    match head {
        Head::Arrow => Some(glyphs.arrow(side)),
        Head::Async => Some(glyphs.open_arrow(side)),
        Head::Cross => Some(glyphs.cross()),
        Head::Open => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msgs(d: &Diagram) -> Vec<(usize, usize, String, Stroke, Head)> {
        d.events
            .iter()
            .filter_map(|e| match e {
                Event::Message {
                    from,
                    to,
                    text,
                    stroke,
                    head,
                    ..
                } => Some((*from, *to, text.clone(), *stroke, *head)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn the_six_arrows_and_their_strokes() {
        let d = parse(
            "sequenceDiagram\n A->>B: a\n A-->>B: b\n A->B: c\n A-->B: d\n A-xB: e\n A--xB: f\n A-)B: g\n A--)B: h",
        )
        .unwrap();
        use Head::*;
        use Stroke::*;
        let got: Vec<(Stroke, Head)> = msgs(&d).into_iter().map(|m| (m.3, m.4)).collect();
        assert_eq!(
            got,
            vec![
                (Solid, Arrow),
                (Dotted, Arrow),
                (Solid, Open),
                (Dotted, Open),
                (Solid, Cross),
                (Dotted, Cross),
                (Solid, Async),
                (Dotted, Async)
            ]
        );
    }

    #[test]
    fn participants_actors_aliases_and_order_of_first_mention() {
        let d = parse(
            "sequenceDiagram\n actor U as The user\n participant S as Server<br/>side\n C->>S: hi\n U->>C: go",
        )
        .unwrap();
        let names: Vec<(&str, &str, bool)> = d
            .participants
            .iter()
            .map(|p| (p.id.as_str(), p.name.as_str(), p.actor))
            .collect();
        assert_eq!(
            names,
            vec![
                ("U", "The user", true),
                ("S", "Server side", false),
                ("C", "C", false)
            ]
        );
    }

    #[test]
    fn activation_shorthand_notes_and_blocks() {
        let d = parse(
            "sequenceDiagram\n A->>+B: x\n B-->>-A: y\n Note over A,B: both\n note left of A: l\n NOTE RIGHT OF B: r\n loop forever\n  alt ok\n   A->>B: z\n  else no\n  end\n end\n rect rgb(0,0,0)\n  A->>A: me\n end\n autonumber",
        )
        .unwrap();
        assert!(d.autonumber);
        assert!(matches!(d.events[0], Event::Message { activate: true, .. }));
        assert!(matches!(
            d.events[1],
            Event::Message {
                deactivate: true,
                ..
            }
        ));
        assert_eq!(
            d.events[2],
            Event::Note {
                at: NoteAt::Over(0, 1),
                text: "both".into()
            }
        );
        assert!(matches!(
            d.events[3],
            Event::Note {
                at: NoteAt::Left(0),
                ..
            }
        ));
        assert!(matches!(
            d.events[4],
            Event::Note {
                at: NoteAt::Right(1),
                ..
            }
        ));
        let kinds: Vec<&str> = d
            .events
            .iter()
            .map(|e| match e {
                Event::Open { .. } => "open",
                Event::Divider { .. } => "divider",
                Event::Close => "close",
                _ => "-",
            })
            .collect();
        assert_eq!(
            kinds,
            vec![
                "-", "-", "-", "-", "-", "open", "open", "-", "divider", "close", "close", "-"
            ],
            "`rect` is read and drawn as nothing"
        );
    }

    #[test]
    fn broken_input_names_its_line() {
        for (src, line) in [
            ("sequenceDiagram\n A->>B: x\n what is this", "line 3"),
            ("sequenceDiagram\n end", "line 2"),
            ("sequenceDiagram\n else", "line 2"),
            ("sequenceDiagram\n Note A: x", "line 2"),
            ("sequenceDiagram\n ->>B: x", "line 2"),
        ] {
            let err = parse(src).unwrap_err();
            assert!(err.starts_with(line), "{src:?}: {err}");
        }
        assert!(parse("sequenceDiagram\n loop x\n A->>B: y").is_err());
    }
}
