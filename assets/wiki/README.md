# The wiki's page

The web page `crystal wiki serve` shows and `crystal wiki export` writes out: one long page about a repository,
after Google's Code Wiki, rendering a `wiki.json` (see `docs/wiki.md` and the contract in `fixture/wiki.json`).
Plain HTML, CSS and JavaScript, no build step and no framework.

| File | What it is |
|---|---|
| `index.html` | the page: the header, the outline, the document, the chat, the zoom and help dialogs, its icons |
| `app.js` | loading the wiki, laying it out, the outline following the reader, diagrams drawn lazily, zoom, Find, Share, the chat's stream, the build's status |
| `markdown.js` | a small, safe markdown renderer (CommonMark's blocks and inlines a wiki uses, GitHub's tables, `code:` links), pure, so node tests it |
| `style.css` | dark and light themes (the system's, or chosen), the three columns, a phone's one, mermaid's drawings recoloured |
| `google-sans-*.woff2`, `OFL-*.txt` | Google Sans Flex (standing in for Google Sans and Google Sans Text, which aren't free) and Google Sans Code, Latin only, under the SIL Open Font License |

`src/wiki_site.rs` builds those into crystal (`PAGE_FILES`, which a test keeps in step with this directory); the
rest here is for working on the page and isn't built in:

- `fixture/wiki.json`: a hand-written wiki about crystal itself, at commit `22ee18c`, four sections of three
  subsections, real paths and lines, flowcharts, a sequence, a state and a class diagram;
- `dev/serve.py`: stands in for `crystal wiki serve` (the page's files, any `wiki.json`, a made-up streaming
  answer for `api/ask`, `api/open` and `api/status`);
- `dev/fixture.py`: the fixture's source, which writes `fixture/wiki.json`;
- `dev/synth.py`: a large synthetic wiki, 16 sections and 90 subsections by default, for `dev/perf.mjs`, which
  times the page in headless Chrome;
- `dev/shoot.mjs` (with `dev/cdp.mjs`): screenshots in headless Chrome, desktop, zoomed, light, phone, chat;
- `test/markdown.test.js`: the renderer's checks, which `tests/wiki_web.rs` runs when node is installed.

```sh
curl -sLo /tmp/mermaid.min.js https://cdn.jsdelivr.net/npm/mermaid@11.17.2/dist/mermaid.min.js
python3 assets/wiki/dev/serve.py --mermaid /tmp/mermaid.min.js   # http://127.0.0.1:8765/p/crystal/
node assets/wiki/test/markdown.test.js
```

How it fits the server: everything is asked for relative to the page (`assets/…`, `wiki.json`, `api/…`), so one
`index.html` works at `/p/<key>/` and exported; a page with `<script type="application/json" id="wiki-data">`
in it (an export) is static, its code links going to the forge and its chat saying asking needs `crystal wiki
serve`. Mermaid 11.17.2 is `assets/mermaid.min.js`, loaded once the first diagram comes near, with dagre's
layout (no ELK). The page's own element ids start with `cw-`, so a section can be called anything.
