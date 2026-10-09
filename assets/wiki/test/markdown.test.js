// Checks the wiki page's markdown renderer: `node assets/wiki/test/markdown.test.js`, which crystal's Rust
// tests run when node is installed (tests/wiki_web.rs).
'use strict';

const test = require('node:test');
const assert = require('node:assert/strict');
const path = require('node:path');
const md = require(path.join(__dirname, '..', 'markdown.js'));

const served = (p, line, end) => ({ href: `https://example.com/blob/abc/${p}${line ? `#L${line}${end ? `-L${end}` : ''}` : ''}`, title: `Open ${p}` });
const render = (src, opts) => md.renderMarkdown(src, { code: served, ...opts });

test('text is escaped, raw HTML included', () => {
  assert.equal(render('a <script>alert(1)</script> & b'), '<p>a &lt;script&gt;alert(1)&lt;/script&gt; &amp; b</p>');
  assert.equal(render('<img src=x onerror=alert(1)>'), '<p>&lt;img src=x onerror=alert(1)&gt;</p>');
  assert.equal(render('line<br>next'), '<p>line<br>next</p>');
});

test('links into the code carry their path and lines', () => {
  const html = render('See [`Session`](code:src/session.rs#L10-L20).');
  assert.match(html, /<a class="code-link" data-path="src\/session.rs" data-line="10" data-end="20" href="https:\/\/example.com\/blob\/abc\/src\/session.rs#L10-L20"/);
  assert.match(html, /><code>Session<\/code><\/a>/);
  assert.match(render('[x](code:src/a.rs#L7)'), /data-line="7" href="[^"]*#L7"/);
  assert.match(render('[x](code:src/a.rs)'), /data-path="src\/a.rs" href="[^"]*src\/a.rs"/);
});

test('a link into the code with nowhere to go is its label alone', () => {
  assert.equal(md.renderMarkdown('[`x`](code:src/a.rs#L1)', { code: () => null }), '<p><code>x</code></p>');
  assert.equal(md.renderMarkdown('[`x`](code:src/a.rs#L1)', {}), '<p><code>x</code></p>');
});

test('a link into the code that only the editor opens is focusable', () => {
  const html = md.renderMarkdown('[y](code:src/a.rs#L3)', { code: () => ({ href: null }) });
  assert.match(html, /<a class="code-link" data-path="src\/a.rs" data-line="3" role="link" tabindex="0">y<\/a>/);
});

test('a relative link is a path in the repository', () => {
  assert.match(render('[readme](docs/guide.md)'), /class="code-link" data-path="docs\/guide.md"/);
});

test('unsafe schemes are dropped, the text kept', () => {
  assert.equal(render('[x](javascript:alert(1))'), '<p>x</p>');
  assert.equal(render('[x](data:text/html,hi)'), '<p>x</p>');
  assert.equal(render('<javascript:alert(1)>'), '<p>&lt;javascript:alert(1)&gt;</p>');
});

test('attributes can not be broken out of', () => {
  const html = render('[x](https://a.b/"onmouseover="alert(1) "t\\"itle")');
  assert.doesNotMatch(html, /"onmouseover=/);
  assert.match(render('[x](https://a.b "say \\"hi\\"")'), /title="say &quot;hi&quot;"/);
});

test('anchors and web links', () => {
  assert.equal(render('[Daemon](#the-daemon)'), '<p><a href="#the-daemon">Daemon</a></p>');
  assert.equal(render('<https://ex.com>'), '<p><a href="https://ex.com" target="_blank" rel="noopener noreferrer">https://ex.com</a></p>');
  assert.equal(render('go to https://ex.com/a.'), '<p>go to <a href="https://ex.com/a" target="_blank" rel="noopener noreferrer">https://ex.com/a</a>.</p>');
  assert.match(render('[r][1]\n\n[1]: https://x.y "T"'), /<a href="https:\/\/x.y" target="_blank" rel="noopener noreferrer" title="T">r<\/a>/);
});

test('an image is a link to it, never loaded', () => {
  assert.equal(render('![chart](https://x/y.png)'), '<p><a href="https://x/y.png" target="_blank" rel="noopener noreferrer">chart</a></p>');
});

test('emphasis', () => {
  assert.equal(render('*a* **b** ***c*** ~~d~~'), '<p><em>a</em> <strong>b</strong> <em><strong>c</strong></em> <del>d</del></p>');
  assert.equal(render('snake_case_name and _em_'), '<p>snake_case_name and <em>em</em></p>');
  assert.equal(render('\\*literal\\* 2 * 3 * 4'), '<p>*literal* 2 * 3 * 4</p>');
  assert.equal(render('**bold `code` here**'), '<p><strong>bold <code>code</code> here</strong></p>');
});

test('code spans', () => {
  assert.equal(render('`a <b>`'), '<p><code>a &lt;b&gt;</code></p>');
  assert.equal(render('`` a`b ``'), '<p><code>a`b</code></p>');
  assert.equal(render('`open'), '<p>`open</p>');
});

test('headings, shifted under the page\'s own', () => {
  assert.equal(render('# One\n## Two'), '<h1>One</h1><h2>Two</h2>');
  assert.equal(render('# One\n###### Six', { headingOffset: 3 }), '<h4>One</h4><h6>Six</h6>');
  assert.equal(render('Title\n===\n\ntext'), '<h1>Title</h1><p>text</p>');
});

test('lists', () => {
  assert.equal(render('- a\n- b\n  - c\n- d'), '<ul><li>a</li><li>b\n<ul><li>c</li></ul></li><li>d</li></ul>');
  assert.equal(render('1. a\n2. b'), '<ol><li>a</li><li>b</li></ol>');
  assert.equal(render('3. c\n4. d'), '<ol start="3"><li>c</li><li>d</li></ol>');
  assert.equal(render('- a\n\n- b'), '<ul><li><p>a</p></li><li><p>b</p></li></ul>');
  assert.equal(render('- [x] done\n- [ ] not'), '<ul><li class="task"><input type="checkbox" disabled checked> done</li><li class="task"><input type="checkbox" disabled> not</li></ul>');
  assert.equal(render('text\n- item'), '<p>text</p><ul><li>item</li></ul>');
});

test('tables', () => {
  const html = render('| Name | What |\n|:--|--:|\n| `a|b` | **x** |');
  assert.equal(
    html,
    '<div class="table-wrap"><table><thead><tr><th style="text-align:left">Name</th><th style="text-align:right">What</th></tr></thead>' +
      '<tbody><tr><td style="text-align:left"><code>a|b</code></td><td style="text-align:right"><strong>x</strong></td></tr></tbody></table></div>',
  );
  assert.equal(render('a | b\n--- | ---\n1 | 2'), '<div class="table-wrap"><table><thead><tr><th>a</th><th>b</th></tr></thead><tbody><tr><td>1</td><td>2</td></tr></tbody></table></div>');
});

test('fenced code is escaped and lightly highlighted', () => {
  assert.equal(
    render('```rust\nfn x() -> &str { "<b>" } // hi\n```'),
    '<pre class="code-block"><code class="language-rust"><span class="tok-k">fn</span> x() -&gt; &amp;str { <span class="tok-s">&quot;&lt;b&gt;&quot;</span> } <span class="tok-c">// hi</span></code></pre>',
  );
  assert.equal(render('```\n<plain>\n```'), '<pre class="code-block"><code>&lt;plain&gt;</code></pre>');
  assert.equal(render('~~~sh\necho 1 # note\n~~~'), '<pre class="code-block"><code class="language-sh"><span class="tok-k">echo</span> <span class="tok-n">1</span> <span class="tok-c"># note</span></code></pre>');
});

test('a mermaid fence becomes a diagram card holding its escaped source', () => {
  assert.equal(render('```mermaid\nflowchart TD\n  a["</pre>"] --> b\n```'), '<figure class="diagram-card" data-mermaid><pre class="mermaid-src">flowchart TD\n  a[&quot;&lt;/pre&gt;&quot;] --&gt; b</pre></figure>');
  assert.match(render('```mermaid\nflowchart TD\n```', { diagrams: false }), /^<pre class="code-block"><code class="language-mermaid">/);
});

test('quotes, rules, breaks and entities', () => {
  assert.equal(render('> quoted\n> on'), '<blockquote><p>quoted\non</p></blockquote>');
  assert.equal(render('a\n\n---\n\nb'), '<p>a</p><hr><p>b</p>');
  assert.equal(render('one  \ntwo\\\nthree'), '<p>one<br>two<br>three</p>');
  assert.equal(render('&lt;b&gt; &amp; &copy; &#65; &bogus;'), '<p>&lt;b&gt; &amp; © A &amp;bogus;</p>');
});

test('code targets and plain text', () => {
  assert.deepEqual(md.parseCodeTarget('src/x.rs#L10-L20'), { path: 'src/x.rs', line: 10, end: 20 });
  assert.deepEqual(md.parseCodeTarget('./src/x.rs#L3'), { path: 'src/x.rs', line: 3, end: null });
  assert.deepEqual(md.parseCodeTarget('src/a%20b.rs'), { path: 'src/a b.rs', line: null, end: null });
  assert.equal(md.plainText('See [`x`](code:a.rs#L1) and **bold**.\n\n```\ncode\n```'), 'See x and bold .');
});
