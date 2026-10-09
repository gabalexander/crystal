#!/usr/bin/env python3
"""Serve the wiki's page the way `crystal wiki serve` does, for working on the page.

    python3 assets/wiki/dev/serve.py --mermaid ~/Downloads/mermaid-11.17.2.min.js
    python3 assets/wiki/dev/serve.py --wiki big=/tmp/big/wiki.json --building

Then open http://127.0.0.1:8765/p/crystal/. It serves the page's files from assets/wiki/ at /assets/ and
/p/<key>/assets/, each wiki given (the fixture, as `crystal`, by default) at /p/<key>/, the list of them at
/projects.json, and stands in for the API: /api/ask streams a made-up answer (ask about "error" for an error,
"slow" for a slow one), /api/open says what it would open, /api/status says what --building and --stale say.
"""

import argparse
import json
import os
import re
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlparse

PAGE = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
TYPES = {
    ".html": "text/html; charset=utf-8",
    ".js": "text/javascript; charset=utf-8",
    ".css": "text/css; charset=utf-8",
    ".json": "application/json",
    ".woff2": "font/woff2",
    ".txt": "text/plain; charset=utf-8",
    ".svg": "image/svg+xml",
}


def answer_for(question, wiki, section):
    files = []
    for sec in wiki.get("sections", []):
        for sub in sec.get("subsections", []):
            if section in (None, sec["id"], sub["id"]):
                files.extend(sub.get("files", []))
    files = files[:3] or ["README.md"]
    first = files[0]
    text = (
        f"Good question. Here is how it works, as far as **{section or 'this repository'}** goes.\n\n"
        f"The entry point is [`{first}`](code:{first}#L1), which sets things up; then:\n\n"
        + "".join(f"- [`{f}`](code:{f}#L{10 * (n + 1)}-L{10 * (n + 1) + 8}) does its part.\n" for n, f in enumerate(files))
        + "\n| Step | Where |\n|---|---|\n| read | `wiki.json` |\n| draw | [`app.js`](code:assets/wiki/app.js#L1) |\n\n"
        + "```rust\nfn main() {\n    // made up by the dev server\n    println!(\"hello\");\n}\n```\n\n"
        + f'You asked: *{question.strip()[:200]}*. Ask a follow-up and it stays in this conversation.'
    )
    return files, text


class Handler(BaseHTTPRequestHandler):
    server_version = "crystal-wiki-dev/1"

    def log_message(self, fmt, *args):
        sys.stderr.write("%s\n" % (fmt % args))

    def send(self, status, body=b"", kind="text/plain; charset=utf-8", headers=None):
        self.send_response(status)
        self.send_header("Content-Type", kind)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        for key, value in (headers or {}).items():
            self.send_header(key, value)
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(body)

    def file(self, path):
        try:
            with open(path, "rb") as f:
                body = f.read()
        except OSError:
            return self.send(404, b"not found")
        self.send(200, body, TYPES.get(os.path.splitext(path)[1], "application/octet-stream"))

    def wiki(self, key):
        path = self.server.wikis.get(key)
        if not path:
            return None
        with open(path, encoding="utf-8") as f:
            return json.load(f)

    def do_HEAD(self):
        self.do_GET()

    def do_GET(self):
        url = urlparse(self.path)
        path = url.path
        if path in ("/", "/projects.json"):
            if path == "/projects.json" or "application/json" in self.headers.get("Accept", ""):
                projects = []
                for key in self.server.wikis:
                    wiki = self.wiki(key)
                    projects.append(
                        {
                            "key": key,
                            "name": wiki["repo"]["name"],
                            "root": wiki["repo"]["root"],
                            "commit": wiki["repo"]["commit"],
                            "updated": wiki["generated"]["at"],
                            "url": f"/p/{key}/",
                        }
                    )
                return self.send(200, json.dumps(projects).encode(), "application/json")
            rows = "".join(f'<li><a href="/p/{key}/">{key}</a></li>' for key in self.server.wikis)
            return self.send(200, f"<!doctype html><h1>Wikis</h1><ul>{rows}</ul>".encode(), TYPES[".html"])
        asset = re.fullmatch(r"(?:/p/[^/]+)?/assets/([^/]+)", path)
        if asset:
            name = asset.group(1)
            if name == "mermaid.min.js":
                if not self.server.mermaid:
                    return self.send(404, b"no --mermaid given")
                return self.file(self.server.mermaid)
            if name.startswith(".") or not os.path.isfile(os.path.join(PAGE, name)):
                return self.send(404, b"not found")
            return self.file(os.path.join(PAGE, name))
        m = re.fullmatch(r"/p/([^/]+)(/.*)?", path)
        if not m or m.group(1) not in self.server.wikis:
            return self.send(404, b"no such wiki")
        key, rest = m.group(1), m.group(2) or ""
        if rest == "":
            return self.send(301, headers={"Location": f"/p/{key}/"})
        if rest in ("/", "/index.html"):
            return self.file(os.path.join(PAGE, "index.html"))
        if rest == "/wiki.json":
            return self.file(self.server.wikis[key])
        if rest == "/api/open":
            query = parse_qs(url.query)
            target = query.get("path", [""])[0]
            if not target or target.startswith("/") or ".." in target.split("/"):
                return self.send(404, b"outside the repository")
            sys.stderr.write(f"would open {target} at line {query.get('line', ['1'])[0]}\n")
            return self.send(204)
        if rest == "/api/status":
            wiki = self.wiki(key)
            status = {
                "building": self.server.building,
                "progress": "12/64 subsections" if self.server.building else None,
                "updated": wiki["generated"]["at"],
                "stale": self.server.stale,
            }
            return self.send(200, json.dumps(status).encode(), "application/json")
        self.send(404, b"not found")

    def do_POST(self):
        m = re.fullmatch(r"/p/([^/]+)/api/ask", urlparse(self.path).path)
        if not m or m.group(1) not in self.server.wikis:
            return self.send(404, b"not found")
        length = int(self.headers.get("Content-Length", "0"))
        try:
            ask = json.loads(self.rfile.read(length) or b"{}")
        except ValueError:
            return self.send(400, b"bad json")
        question = str(ask.get("question") or "")
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-store")
        self.end_headers()

        def event(name, data):
            self.wfile.write(f"event: {name}\ndata: {json.dumps(data)}\n\n".encode())
            self.wfile.flush()

        try:
            if "error" in question.lower():
                time.sleep(0.4)
                return event("error", {"message": "claude exited with status 1: the dev server was asked for an error"})
            files, text = answer_for(question, self.wiki(m.group(1)), ask.get("section"))
            for f in files:
                time.sleep(0.25)
                event("tool", {"name": "Read", "path": f})
            delay = 0.12 if "slow" in question.lower() else 0.02
            for chunk in re.findall(r"\S+\s*", text):
                time.sleep(delay)
                event("delta", {"text": chunk})
            event("done", {"conversation": ask.get("conversation") or "dev-conversation-1", "cost_usd": 0.03})
        except (BrokenPipeError, ConnectionResetError):
            pass


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--port", type=int, default=8765)
    parser.add_argument("--mermaid", help="mermaid.min.js, served at /assets/mermaid.min.js")
    parser.add_argument("--wiki", action="append", default=[], help="KEY=PATH of a wiki.json; the fixture by default")
    parser.add_argument("--building", action="store_true", help="say a build is under way")
    parser.add_argument("--stale", action="store_true", help="say the default branch has moved since")
    args = parser.parse_args()
    wikis = dict(w.split("=", 1) for w in args.wiki) or {"crystal": os.path.join(PAGE, "fixture", "wiki.json")}
    server = ThreadingHTTPServer(("127.0.0.1", args.port), Handler)
    server.wikis = wikis
    server.mermaid = args.mermaid or os.environ.get("CRYSTAL_WIKI_MERMAID")
    server.building = args.building
    server.stale = args.stale
    first = next(iter(wikis))
    sys.stderr.write(f"serving http://127.0.0.1:{args.port}/p/{first}/\n")
    server.serve_forever()


if __name__ == "__main__":
    main()
