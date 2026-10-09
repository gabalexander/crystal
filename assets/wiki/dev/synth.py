#!/usr/bin/env python3
"""Make a large synthetic wiki.json, for checking the page's performance.

    python3 assets/wiki/dev/synth.py --sections 16 --subsections 90 > /tmp/big/wiki.json

Its text is made up, but shaped like a real wiki's: each section and subsection with a diagram (flowcharts
top to bottom and left to right, a sequence diagram now and then) and paragraphs, lists, a table or a code
block, with about thirty links into the code each, as Code Wiki's pages have.
"""

import argparse
import json
import random

WORDS = (
    "session daemon socket worker queue buffer screen viewer listener request response event bus plugin hook "
    "task flow step runner parser reader writer cache index store entry table record frame packet stream "
    "channel handle state config profile layout pane tab sidebar theme key command client server process "
    "child signal timer watcher loader linker symbol module package compiler optimizer scheduler allocator"
).split()
VERBS = "reads writes owns starts stops hands parses keeps sends checks builds loads draws tells waits lays".split()
DIRS = ["src", "src/tui", "src/daemon", "src/mermaid", "src/forge", "src/api", "tests", "docs"]


def words(rng, n):
    return " ".join(rng.choice(WORDS) for _ in range(n))


def path(rng):
    return f"{rng.choice(DIRS)}/{rng.choice(WORDS)}_{rng.choice(WORDS)}.rs"


def code_link(rng):
    name = rng.choice(
        [
            f"{rng.choice(WORDS).capitalize()}{rng.choice(WORDS).capitalize()}",
            f"{rng.choice(WORDS)}_{rng.choice(WORDS)}()",
            path(rng),
            f"--{rng.choice(WORDS)}",
        ]
    )
    line = rng.randint(1, 2000)
    end = f"-L{line + rng.randint(1, 40)}" if rng.random() < 0.4 else ""
    return f"[`{name}`](code:{path(rng)}#L{line}{end})"


def sentence(rng, links):
    parts = [words(rng, rng.randint(3, 8)).capitalize()]
    for _ in range(links):
        parts.append(f"{rng.choice(VERBS)} {code_link(rng)}")
        parts.append(words(rng, rng.randint(2, 7)))
    return " ".join(parts) + "."


def paragraph(rng, sentences=4, links=2):
    return " ".join(sentence(rng, rng.randint(1, links)) for _ in range(sentences))


def label(rng):
    return f"{rng.choice(WORDS).capitalize()} {rng.choice(WORDS).capitalize()}"


def flowchart(rng, nodes):
    direction = rng.choice(["TD", "TD", "LR"])
    lines = [f"flowchart {direction}"]
    ids = [f"n{i}" for i in range(nodes)]
    for node in ids:
        lines.append(f'  {node}["{label(rng)}<br/>({path(rng)})"]')
    for i in range(1, nodes):
        source = ids[rng.randrange(0, i)]
        arrow = "-.->" if rng.random() < 0.25 else "-->"
        lines.append(f"  {source} {arrow}|{rng.choice(VERBS)} {rng.choice(WORDS)}| {ids[i]}")
    if nodes > 3 and rng.random() < 0.5:
        lines.append(f"  {ids[-1]} -->|{rng.choice(VERBS)}| {ids[1]}")
    return "\n".join(lines)


def sequence(rng):
    actors = [label(rng).replace(" ", "") for _ in range(rng.randint(3, 4))]
    lines = ["sequenceDiagram"] + [f"  participant {a}" for a in actors]
    for _ in range(rng.randint(4, 7)):
        a, b = rng.sample(actors, 2)
        arrow = rng.choice(["->>", "-->>"])
        lines.append(f"  {a}{arrow}{b}: {rng.choice(VERBS)} {rng.choice(WORDS)}")
    return "\n".join(lines)


def diagram(rng):
    source = sequence(rng) if rng.random() < 0.15 else flowchart(rng, rng.randint(4, 8))
    return {"mermaid": source, "caption": f"How the {words(rng, 2)} fits together"}


def body(rng):
    blocks = [paragraph(rng, 4, 4), paragraph(rng, 3, 3)]
    blocks.append("\n".join(f"- **{label(rng)}**: {sentence(rng, 1)}" for _ in range(rng.randint(3, 5))))
    if rng.random() < 0.3:
        rows = "\n".join(f"| {code_link(rng)} | {words(rng, 6)} |" for _ in range(4))
        blocks.append(f"| Name | What it does |\n|---|---|\n{rows}")
    if rng.random() < 0.3:
        blocks.append(f'```rust\nfn {rng.choice(WORDS)}(x: u32) -> String {{\n    // {words(rng, 5)}\n    format!("{{x}}")\n}}\n```')
    blocks.append(paragraph(rng, 4, 3))
    return "\n\n".join(blocks)


def slug(text, seen):
    base = "-".join("".join(c if c.isalnum() else " " for c in text.lower()).split())
    out, n = base, 2
    while out in seen:
        out, n = f"{base}-{n}", n + 1
    seen.add(out)
    return out


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--sections", type=int, default=16)
    parser.add_argument("--subsections", type=int, default=90, help="in all, spread over the sections")
    parser.add_argument("--seed", type=int, default=7)
    args = parser.parse_args()
    rng = random.Random(args.seed)
    seen = set()
    sections = []
    for s in range(args.sections):
        count = args.subsections // args.sections + (1 if s < args.subsections % args.sections else 0)
        title = f"{label(rng)} and {label(rng)}"
        subsections = []
        for _ in range(count):
            sub_title = f"The {label(rng)}: {words(rng, 3)}"
            subsections.append(
                {
                    "id": slug(sub_title, seen),
                    "title": sub_title,
                    "body_md": body(rng),
                    "diagram": diagram(rng),
                    "files": [path(rng) for _ in range(rng.randint(1, 4))],
                }
            )
        sections.append(
            {
                "id": slug(title, seen),
                "title": title,
                "summary_md": paragraph(rng, 5, 2),
                "diagram": diagram(rng),
                "subsections": subsections,
            }
        )
    links = " ".join(f"[{s['title']}](#{s['id']})" for s in sections[:6])
    wiki = {
        "version": 1,
        "repo": {
            "name": "example/synthetic",
            "root": "/tmp/synthetic",
            "commit": "0123456789abcdef0123456789abcdef01234567",
            "branch": "main",
            "web_url": "https://github.com/example/synthetic",
            "code_url": "https://github.com/example/synthetic/blob/{commit}/{path}",
        },
        "generated": {
            "at": "2026-10-09T12:00:00Z",
            "by": "Claude Sonnet 5.5",
            "model": "claude-sonnet-5-5",
            "cost_usd": 12.5,
            "crystal": "0.3.0",
        },
        "overview": {
            "summary_md": f"{paragraph(rng, 5, 2)}\n\n{paragraph(rng, 4, 2)}\n\nRead on: {links}.",
            "diagram": diagram(rng),
        },
        "sections": sections,
    }
    print(json.dumps(wiki, indent=1))


if __name__ == "__main__":
    main()
