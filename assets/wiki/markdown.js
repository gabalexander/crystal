// A small, safe markdown renderer for the wiki's page and its chat: CommonMark's blocks and inlines that a
// wiki uses (paragraphs, headings, lists, block quotes, fenced code with light highlighting, GitHub's tables,
// links, emphasis and code spans), everything else escaped. Raw HTML is never passed through: a tag in the
// text is shown as text, but for <br>.
//
// Links into the code are written `[label](code:PATH#L10-L20)`; the renderer hands each to `opts.code`, which
// says where it goes, and marks it with `data-path` and `data-line` for the page to open it. A ```mermaid
// fence becomes a diagram card the page draws. Pure: a string in, a string of HTML out, so node tests it.
(function (root) {
  'use strict';

  const ESCAPES = { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' };

  function escapeHtml(text) {
    return String(text).replace(/[&<>"']/g, (c) => ESCAPES[c]);
  }

  const PUNCT = /[!-/:-@[-`{-~ -⁯⸀-⹿　-〿]/;
  const isPunct = (c) => c !== '' && PUNCT.test(c);
  const isSpace = (c) => c === '' || /\s/.test(c);

  const ENTITIES = { amp: '&', lt: '<', gt: '>', quot: '"', apos: "'", nbsp: ' ', copy: '©', mdash: '—', ndash: '–', hellip: '…', rarr: '→', larr: '←', times: '×' };

  function decodeEntity(entity) {
    if (entity[1] === '#') {
      const code = entity[2] === 'x' || entity[2] === 'X' ? parseInt(entity.slice(3, -1), 16) : parseInt(entity.slice(2, -1), 10);
      if (!code || code > 0x10ffff || (code >= 0xd800 && code <= 0xdfff)) return '�';
      return String.fromCodePoint(code);
    }
    const name = entity.slice(1, -1);
    return Object.prototype.hasOwnProperty.call(ENTITIES, name) ? ENTITIES[name] : null;
  }

  // ---- Links ------------------------------------------------------------------------------------------

  // A `code:` destination read into its path and lines: `src/x.rs#L10-L20` is lines 10 to 20.
  function parseCodeTarget(target) {
    const hash = target.indexOf('#');
    let path = hash < 0 ? target : target.slice(0, hash);
    const frag = hash < 0 ? '' : target.slice(hash + 1);
    path = path.replace(/^\.?\/+/, '');
    try {
      path = decodeURIComponent(path);
    } catch (_) {
      // Kept as written.
    }
    const lines = /^L(\d+)(?:-L?(\d+))?$/.exec(frag);
    return { path, line: lines ? Number(lines[1]) : null, end: lines && lines[2] ? Number(lines[2]) : null };
  }

  // What a link's destination becomes: an anchor on the page, a link into the code, a web or mail link, or
  // nothing (an unknown scheme like `javascript:` is shown as its text alone). A relative path is taken as a
  // path in the repository, as a wiki's links are.
  function resolveLink(dest, opts) {
    const href = dest.trim();
    if (href.startsWith('#')) return { kind: 'anchor', href };
    const scheme = /^([a-zA-Z][a-zA-Z0-9+.-]*):/.exec(href);
    if (scheme) {
      const name = scheme[1].toLowerCase();
      if (name === 'code') return { kind: 'code', ...parseCodeTarget(href.slice(5)) };
      if (name === 'http' || name === 'https' || name === 'mailto') return { kind: 'web', href };
      return { kind: 'none' };
    }
    if (href.startsWith('//')) return { kind: 'none' };
    if (href === '') return { kind: 'none' };
    return { kind: 'code', ...parseCodeTarget(href) };
  }

  function linkHtml(target, inner, title, opts) {
    const titleAttr = title ? ` title="${escapeHtml(title)}"` : '';
    switch (target.kind) {
      case 'anchor':
        return `<a href="${escapeHtml(target.href)}"${titleAttr}>${inner}</a>`;
      case 'web':
        return `<a href="${escapeHtml(target.href)}" target="_blank" rel="noopener noreferrer"${titleAttr}>${inner}</a>`;
      case 'code': {
        const where = opts.code ? opts.code(target.path, target.line, target.end) : null;
        if (!where) return inner;
        let attrs = ` data-path="${escapeHtml(target.path)}"`;
        if (target.line) attrs += ` data-line="${target.line}"`;
        if (target.end) attrs += ` data-end="${target.end}"`;
        if (where.href) attrs += ` href="${escapeHtml(where.href)}" target="_blank" rel="noopener noreferrer"`;
        else attrs += ' role="link" tabindex="0"';
        const hint = where.title || title;
        if (hint) attrs += ` title="${escapeHtml(hint)}"`;
        return `<a class="code-link"${attrs}>${inner}</a>`;
      }
      default:
        return inner;
    }
  }

  // ---- Inlines ----------------------------------------------------------------------------------------

  // Where the `[` at `start` is closed, skipping code spans, escapes and nested brackets.
  function findLabelEnd(src, start) {
    let depth = 0;
    for (let i = start; i < src.length; i++) {
      const c = src[i];
      if (c === '\\') {
        i++;
      } else if (c === '`') {
        const run = /^`+/.exec(src.slice(i))[0];
        const close = findCodeClose(src, i + run.length, run.length);
        if (close >= 0) i = close + run.length - 1;
        else i += run.length - 1;
      } else if (c === '[') {
        depth++;
      } else if (c === ']') {
        depth--;
        if (depth === 0) return i;
      }
    }
    return -1;
  }

  function findCodeClose(src, from, n) {
    let i = from;
    while (i < src.length) {
      const at = src.indexOf('`', i);
      if (at < 0) return -1;
      let end = at;
      while (src[end] === '`') end++;
      if (end - at === n) return at;
      i = end;
    }
    return -1;
  }

  // An inline link's `(destination "title")`, from the `(` at `start`: its destination, title and end.
  function parseInlineDest(src, start) {
    let i = start + 1;
    while (i < src.length && /[ \t\n]/.test(src[i])) i++;
    let dest = '';
    if (src[i] === '<') {
      const close = src.indexOf('>', i);
      if (close < 0 || src.slice(i + 1, close).includes('\n')) return null;
      dest = src.slice(i + 1, close);
      i = close + 1;
    } else {
      let depth = 0;
      const from = i;
      while (i < src.length) {
        const c = src[i];
        if (c === '\\' && i + 1 < src.length) {
          i += 2;
          continue;
        }
        if (/\s/.test(c)) break;
        if (c === '(') depth++;
        if (c === ')') {
          if (depth === 0) break;
          depth--;
        }
        i++;
      }
      dest = src.slice(from, i).replace(/\\([!-/:-@[-`{-~])/g, '$1');
    }
    while (i < src.length && /[ \t\n]/.test(src[i])) i++;
    let title = null;
    const open = src[i];
    if (open === '"' || open === "'" || open === '(') {
      const closeChar = open === '(' ? ')' : open;
      let close = i + 1;
      while (close < src.length && src[close] !== closeChar) close += src[close] === '\\' ? 2 : 1;
      if (close >= src.length) return null;
      title = src.slice(i + 1, close).replace(/\\([!-/:-@[-`{-~])/g, '$1');
      i = close + 1;
      while (i < src.length && /[ \t\n]/.test(src[i])) i++;
    }
    if (src[i] !== ')') return null;
    return { dest, title, end: i + 1 };
  }

  function normalizeLabel(label) {
    return label.trim().replace(/\s+/g, ' ').toLowerCase();
  }

  function renderInline(src, opts, refs, inLink) {
    const nodes = [];
    let text = '';
    const flush = () => {
      if (text) nodes.push({ t: 'text', v: text });
      text = '';
    };
    const html = (v) => {
      flush();
      nodes.push({ t: 'html', v });
    };
    let i = 0;
    while (i < src.length) {
      const c = src[i];
      if (c === '\\') {
        const next = src[i + 1];
        if (next === '\n') {
          html('<br>');
          i += 2;
        } else if (next !== undefined && isPunct(next)) {
          text += next;
          i += 2;
        } else {
          text += c;
          i++;
        }
        continue;
      }
      if (c === '`') {
        const run = /^`+/.exec(src.slice(i))[0];
        const close = findCodeClose(src, i + run.length, run.length);
        if (close < 0) {
          text += run;
          i += run.length;
          continue;
        }
        let code = src.slice(i + run.length, close).replace(/\n/g, ' ');
        if (code.length > 2 && code[0] === ' ' && code[code.length - 1] === ' ' && code.trim()) code = code.slice(1, -1);
        html(`<code>${escapeHtml(code)}</code>`);
        i = close + run.length;
        continue;
      }
      if (c === '<') {
        const rest = src.slice(i);
        const br = /^<br\s*\/?>/i.exec(rest);
        if (br) {
          html('<br>');
          i += br[0].length;
          continue;
        }
        const auto = /^<([a-zA-Z][a-zA-Z0-9+.-]{1,31}:[^\s<>]*)>/.exec(rest);
        if (auto && !inLink) {
          const target = resolveLink(auto[1], opts);
          html(target.kind === 'none' ? escapeHtml(auto[0]) : linkHtml(target, escapeHtml(auto[1]), null, opts));
          i += auto[0].length;
          continue;
        }
        const mail = /^<([a-zA-Z0-9.!#$%&'*+/=?^_`{|}~-]+@[a-zA-Z0-9](?:[a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?(?:\.[a-zA-Z0-9](?:[a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?)*)>/.exec(rest);
        if (mail && !inLink) {
          html(linkHtml({ kind: 'web', href: `mailto:${mail[1]}` }, escapeHtml(mail[1]), null, opts));
          i += mail[0].length;
          continue;
        }
        text += c;
        i++;
        continue;
      }
      if ((c === '[' || (c === '!' && src[i + 1] === '[')) && !inLink) {
        const image = c === '!';
        const open = image ? i + 1 : i;
        const close = findLabelEnd(src, open);
        if (close > 0) {
          const label = src.slice(open + 1, close);
          let target = null;
          let title = null;
          let end = close + 1;
          if (src[close + 1] === '(') {
            const dest = parseInlineDest(src, close + 1);
            if (dest) {
              target = resolveLink(dest.dest, opts);
              title = dest.title;
              end = dest.end;
            }
          }
          if (!target) {
            let ref = label;
            const full = /^\[([^\]]*)\]/.exec(src.slice(close + 1));
            if (full) {
              if (full[1].trim()) ref = full[1];
              end = close + 1 + full[0].length;
            }
            const def = refs[normalizeLabel(ref)];
            if (def) {
              target = resolveLink(def.dest, opts);
              title = def.title;
            } else {
              end = close + 1;
            }
          }
          if (target) {
            // An image is shown as a link to it: the page loads nothing from elsewhere.
            const inner = image ? escapeHtml(label || 'image') : renderInline(label, opts, refs, true);
            html(target.kind === 'none' ? inner : linkHtml(target, inner, title, opts));
            i = end;
            continue;
          }
        }
        text += c;
        i++;
        continue;
      }
      if (c === '*' || c === '_' || c === '~') {
        let end = i;
        while (src[end] === c) end++;
        const n = end - i;
        const before = i > 0 ? src[i - 1] : '';
        const after = end < src.length ? src[end] : '';
        const left = !isSpace(after) && (!isPunct(after) || isSpace(before) || isPunct(before));
        const right = !isSpace(before) && (!isPunct(before) || isSpace(after) || isPunct(after));
        let canOpen = left;
        let canClose = right;
        if (c === '_') {
          canOpen = left && (!right || isPunct(before));
          canClose = right && (!left || isPunct(after));
        }
        if (c === '~' && n !== 2) {
          canOpen = false;
          canClose = false;
        }
        flush();
        nodes.push({ t: 'delim', ch: c, n, orig: n, canOpen, canClose });
        i = end;
        continue;
      }
      if (c === '\n') {
        if (/ {2,}$/.test(text)) {
          text = text.replace(/ +$/, '');
          html('<br>');
        } else {
          text = text.replace(/ +$/, '');
          text += '\n';
        }
        i++;
        while (src[i] === ' ') i++;
        continue;
      }
      if (c === '&') {
        const entity = /^&(?:#\d{1,7}|#[xX][0-9a-fA-F]{1,6}|[a-zA-Z][a-zA-Z0-9]{1,31});/.exec(src.slice(i));
        if (entity) {
          const decoded = decodeEntity(entity[0]);
          if (decoded !== null) {
            text += decoded;
            i += entity[0].length;
            continue;
          }
        }
        text += c;
        i++;
        continue;
      }
      if ((c === 'h' || c === 'w') && !inLink && (i === 0 || /[\s(*_~]/.test(src[i - 1]))) {
        const bare = /^(?:https?:\/\/|www\.)[^\s<]*[^\s<?!.,:;*_~)'"]/.exec(src.slice(i));
        if (bare) {
          const href = bare[0].startsWith('www.') ? `https://${bare[0]}` : bare[0];
          html(linkHtml({ kind: 'web', href }, escapeHtml(bare[0]), null, opts));
          i += bare[0].length;
          continue;
        }
      }
      text += c;
      i++;
    }
    flush();
    processEmphasis(nodes);
    let out = '';
    for (const node of nodes) {
      if (node.t === 'text') out += escapeHtml(node.v);
      else if (node.t === 'html') out += node.v;
      else out += escapeHtml(node.ch.repeat(node.n));
    }
    return out;
  }

  // CommonMark's emphasis: each closing run matched with the nearest opener of its kind before it.
  function processEmphasis(nodes) {
    for (let ci = 0; ci < nodes.length; ci++) {
      const closer = nodes[ci];
      if (closer.t !== 'delim' || !closer.canClose || closer.n === 0) continue;
      let matched = false;
      for (let oi = ci - 1; oi >= 0; oi--) {
        const opener = nodes[oi];
        if (opener.t !== 'delim' || opener.ch !== closer.ch || !opener.canOpen || opener.n === 0) continue;
        const odd = (opener.canClose || closer.canOpen) && (opener.orig + closer.orig) % 3 === 0 && !(opener.orig % 3 === 0 && closer.orig % 3 === 0);
        if (odd && closer.ch !== '~') continue;
        const use = closer.ch === '~' ? 2 : closer.n >= 2 && opener.n >= 2 ? 2 : 1;
        const tag = closer.ch === '~' ? 'del' : use === 2 ? 'strong' : 'em';
        for (let k = oi + 1; k < ci; k++) {
          if (nodes[k].t === 'delim') nodes[k] = { t: 'text', v: nodes[k].ch.repeat(nodes[k].n) };
        }
        opener.n -= use;
        closer.n -= use;
        nodes.splice(ci, 0, { t: 'html', v: `</${tag}>` });
        nodes.splice(oi + 1, 0, { t: 'html', v: `<${tag}>` });
        ci += 1;
        matched = true;
        if (closer.n > 0) ci -= 1;
        break;
      }
      if (!matched && !closer.canOpen) closer.canClose = false;
    }
  }

  // ---- Highlighting -----------------------------------------------------------------------------------

  const KEYWORDS = {
    c: 'as async await break case catch class const continue crate default defer do dyn else enum export extends false fn for func function go goto if impl import in interface let loop match mod move mut new nil null package pub return select self Self static struct super switch this throw trait true try type typeof undefined unsafe use var void where while yield',
    py: 'and as assert async await break class continue def del elif else except False finally for from global if import in is lambda None nonlocal not or pass raise return True try while with yield',
    sh: 'case do done elif else esac export fi for function if in local return then until while echo cd set unset',
    toml: 'true false',
  };
  const LANGS = {
    rust: 'c', rs: 'c', go: 'c', js: 'c', javascript: 'c', ts: 'c', typescript: 'c', jsx: 'c', tsx: 'c', c: 'c', h: 'c', cpp: 'c', java: 'c', kotlin: 'c', swift: 'c', json: 'c', jsonc: 'c', css: 'c', scala: 'c', zig: 'c',
    python: 'py', py: 'py', ruby: 'py', rb: 'py',
    sh: 'sh', bash: 'sh', zsh: 'sh', shell: 'sh', console: 'sh', fish: 'sh',
    toml: 'toml', yaml: 'toml', yml: 'toml', ini: 'toml', make: 'sh', makefile: 'sh', dockerfile: 'sh',
  };

  const tokenCache = {};
  function tokenizer(family) {
    if (tokenCache[family]) return tokenCache[family];
    const comment = family === 'c' ? String.raw`\/\/[^\n]*|\/\*[\s\S]*?\*\/` : String.raw`#[^\n]*`;
    const string = String.raw`"(?:\\[\s\S]|[^"\\\n])*"|'(?:\\[\s\S]|[^'\\\n]){0,2}'` + (family === 'c' ? '|`[^`]*`' : String.raw`|'(?:\\[\s\S]|[^'\\\n])*'`);
    const re = new RegExp(`(${comment})|(${string})|(\\b\\d[\\d_]*(?:\\.\\d+)?(?:[eE][+-]?\\d+)?[a-z0-9]*\\b|\\b0x[0-9a-fA-F_]+\\b)|([A-Za-z_][A-Za-z0-9_]*)`, 'g');
    const words = new Set((KEYWORDS[family] || '').split(' '));
    tokenCache[family] = { re, words };
    return tokenCache[family];
  }

  function highlight(code, lang) {
    const family = LANGS[(lang || '').toLowerCase()];
    if (!family) return escapeHtml(code);
    const { re, words } = tokenizer(family);
    re.lastIndex = 0;
    let out = '';
    let last = 0;
    let m;
    while ((m = re.exec(code))) {
      out += escapeHtml(code.slice(last, m.index));
      const [token, comment, string, number, word] = m;
      if (comment) out += `<span class="tok-c">${escapeHtml(token)}</span>`;
      else if (string) out += `<span class="tok-s">${escapeHtml(token)}</span>`;
      else if (number) out += `<span class="tok-n">${escapeHtml(token)}</span>`;
      else if (word && words.has(word)) out += `<span class="tok-k">${escapeHtml(token)}</span>`;
      else out += escapeHtml(token);
      last = m.index + token.length;
    }
    return out + escapeHtml(code.slice(last));
  }

  // ---- Blocks -----------------------------------------------------------------------------------------

  const FENCE = /^( {0,3})(`{3,}|~{3,})[ \t]*([^`\s]*)[^`]*$/;
  const HEADING = /^ {0,3}(#{1,6})(?:[ \t]+(.*?))?(?:[ \t]+#+)?[ \t]*$/;
  const RULE = /^ {0,3}([-*_])(?:[ \t]*\1){2,}[ \t]*$/;
  const QUOTE = /^ {0,3}> ?/;
  const ITEM = /^( {0,3})([-+*]|\d{1,9}[.)])(?=[ \t]|$)([ \t]*)/;
  const TABLE_DELIM = /^ {0,3}\|?[ \t]*:?-+:?[ \t]*(?:\|[ \t]*:?-+:?[ \t]*)*\|?[ \t]*$/;
  const DEFINITION = /^ {0,3}\[([^\]]+)\]:[ \t]*<?([^\s>]+)>?(?:[ \t]+(?:"([^"]*)"|'([^']*)'|\(([^)]*)\)))?[ \t]*$/;

  const isBlank = (line) => /^[ \t]*$/.test(line);

  function expandTabs(line) {
    if (!line.includes('\t')) return line;
    let out = '';
    for (const c of line) {
      if (c === '\t') out += ' '.repeat(4 - (out.length % 4));
      else out += c;
    }
    return out;
  }

  function indentOf(line) {
    return /^ */.exec(line)[0].length;
  }

  function splitRow(line) {
    let row = line.trim();
    if (row.startsWith('|')) row = row.slice(1);
    if (row.endsWith('|') && !row.endsWith('\\|')) row = row.slice(0, -1);
    const cells = [];
    let cell = '';
    let inCode = 0;
    for (let i = 0; i < row.length; i++) {
      const c = row[i];
      if (c === '\\' && row[i + 1] === '|') {
        cell += '|';
        i++;
      } else if (c === '`') {
        let n = 0;
        while (row[i + n] === '`') n++;
        if (inCode === 0) inCode = n;
        else if (inCode === n) inCode = 0;
        cell += '`'.repeat(n);
        i += n - 1;
      } else if (c === '|' && inCode === 0) {
        cells.push(cell.trim());
        cell = '';
      } else {
        cell += c;
      }
    }
    cells.push(cell.trim());
    return cells;
  }

  // Whether a line starts a block that ends the paragraph before it.
  function interrupts(line) {
    if (FENCE.test(line) || HEADING.test(line) || QUOTE.test(line) || RULE.test(line)) return true;
    const item = ITEM.exec(line);
    if (item && !isBlank(line.slice(item[0].length))) return !/^\d/.test(item[2]) || /^1[.)]$/.test(item[2]);
    return false;
  }

  function renderBlocks(lines, ctx, tight) {
    const out = [];
    let i = 0;
    while (i < lines.length) {
      const line = lines[i];
      if (isBlank(line)) {
        i++;
        continue;
      }
      const fence = FENCE.exec(line);
      if (fence) {
        const indent = fence[1].length;
        const marker = fence[2];
        const lang = fence[3].replace(/[{}.]/g, '');
        const body = [];
        i++;
        while (i < lines.length) {
          const close = new RegExp(`^ {0,3}${marker[0] === '`' ? '`' : '~'}{${marker.length},}[ \\t]*$`).exec(lines[i]);
          if (close) {
            i++;
            break;
          }
          body.push(lines[i].replace(new RegExp(`^ {0,${indent}}`), ''));
          i++;
        }
        const code = body.join('\n');
        if (lang.toLowerCase() === 'mermaid' && ctx.opts.diagrams !== false) {
          out.push(`<figure class="diagram-card" data-mermaid><pre class="mermaid-src">${escapeHtml(code)}</pre></figure>`);
        } else {
          const cls = lang ? ` class="language-${escapeHtml(lang)}"` : '';
          out.push(`<pre class="code-block"><code${cls}>${highlight(code, lang)}</code></pre>`);
        }
        continue;
      }
      const heading = HEADING.exec(line);
      if (heading) {
        const level = Math.min(6, heading[1].length + (ctx.opts.headingOffset || 0));
        out.push(`<h${level}>${renderInline(heading[2] || '', ctx.opts, ctx.refs, false)}</h${level}>`);
        i++;
        continue;
      }
      if (RULE.test(line)) {
        out.push('<hr>');
        i++;
        continue;
      }
      if (QUOTE.test(line)) {
        const body = [];
        while (i < lines.length && !isBlank(lines[i])) {
          if (QUOTE.test(lines[i])) body.push(lines[i].replace(QUOTE, ''));
          else if (interrupts(lines[i])) break;
          else body.push(lines[i]);
          i++;
        }
        out.push(`<blockquote>${renderBlocks(body, ctx, false)}</blockquote>`);
        continue;
      }
      const item = ITEM.exec(line);
      if (item) {
        i = renderList(lines, i, ctx, out);
        continue;
      }
      if (line.includes('|') && i + 1 < lines.length && TABLE_DELIM.test(lines[i + 1]) && lines[i + 1].includes('-')) {
        const head = splitRow(line);
        const aligns = splitRow(lines[i + 1]).map((cell) => (cell.startsWith(':') && cell.endsWith(':') ? 'center' : cell.endsWith(':') ? 'right' : cell.startsWith(':') ? 'left' : ''));
        if (aligns.length === head.length) {
          i += 2;
          const rows = [];
          while (i < lines.length && !isBlank(lines[i]) && !interrupts(lines[i])) {
            rows.push(splitRow(lines[i]));
            i++;
          }
          const cell = (tag, text, col) => {
            const align = aligns[col] ? ` style="text-align:${aligns[col]}"` : '';
            return `<${tag}${align}>${renderInline(text || '', ctx.opts, ctx.refs, false)}</${tag}>`;
          };
          const thead = `<thead><tr>${head.map((text, col) => cell('th', text, col)).join('')}</tr></thead>`;
          const tbody = rows.length ? `<tbody>${rows.map((row) => `<tr>${head.map((_, col) => cell('td', row[col], col)).join('')}</tr>`).join('')}</tbody>` : '';
          out.push(`<div class="table-wrap"><table>${thead}${tbody}</table></div>`);
          continue;
        }
      }
      if (indentOf(line) >= 4 && !tight) {
        const body = [];
        while (i < lines.length && (indentOf(lines[i]) >= 4 || isBlank(lines[i]))) {
          body.push(lines[i].slice(4));
          i++;
        }
        while (body.length && isBlank(body[body.length - 1])) body.pop();
        out.push(`<pre class="code-block"><code>${escapeHtml(body.join('\n'))}</code></pre>`);
        continue;
      }
      // A paragraph, or a heading underlined with = or -.
      const para = [line.trimStart()];
      i++;
      let setext = 0;
      while (i < lines.length && !isBlank(lines[i])) {
        if (/^ {0,3}=+[ \t]*$/.test(lines[i])) {
          setext = 1;
          i++;
          break;
        }
        if (/^ {0,3}-+[ \t]*$/.test(lines[i])) {
          setext = 2;
          i++;
          break;
        }
        if (interrupts(lines[i])) break;
        para.push(lines[i].trimStart());
        i++;
      }
      const inner = renderInline(para.join('\n').trimEnd(), ctx.opts, ctx.refs, false);
      if (setext) {
        const level = Math.min(6, setext + (ctx.opts.headingOffset || 0));
        out.push(`<h${level}>${inner}</h${level}>`);
      } else {
        out.push(tight ? inner : `<p>${inner}</p>`);
      }
    }
    return out.join(tight ? '\n' : '');
  }

  // A list from line `start`: its items, each its lines with the marker's indent taken off, rendered as
  // blocks; tight (no paragraphs) unless a blank line separates its items or the blocks in one.
  function renderList(lines, start, ctx, out) {
    const first = ITEM.exec(lines[start]);
    const ordered = /^\d/.test(first[2]);
    const delim = first[2].slice(-1);
    const items = [];
    let loose = false;
    let i = start;
    while (i < lines.length) {
      const m = ITEM.exec(lines[i]);
      if (!m) break;
      if (/^\d/.test(m[2]) !== ordered || m[2].slice(-1) !== delim) break;
      const rest = lines[i].slice(m[0].length);
      const spacing = m[3].length;
      const width = m[1].length + m[2].length + (spacing >= 1 && spacing <= 4 && !isBlank(rest) ? spacing : 1);
      const body = [isBlank(rest) ? '' : ' '.repeat(Math.max(0, spacing - (width - m[1].length - m[2].length))) + rest];
      i++;
      let sawBlank = false;
      while (i < lines.length) {
        const line = lines[i];
        if (isBlank(line)) {
          body.push('');
          sawBlank = true;
          i++;
          continue;
        }
        if (indentOf(line) >= width) {
          if (sawBlank && body.some((l) => !isBlank(l))) loose = true;
          body.push(line.slice(width));
          sawBlank = false;
          i++;
          continue;
        }
        if (!sawBlank && !ITEM.test(line) && !interrupts(line) && !isBlank(body[body.length - 1] || '')) {
          body.push(line.trimStart());
          i++;
          continue;
        }
        break;
      }
      while (body.length && isBlank(body[body.length - 1])) body.pop();
      if (sawBlank && i < lines.length) {
        const next = ITEM.exec(lines[i]);
        if (next && /^\d/.test(next[2]) === ordered && next[2].slice(-1) === delim) loose = true;
      }
      items.push({ body, number: ordered ? parseInt(m[2], 10) : null });
    }
    const tag = ordered ? 'ol' : 'ul';
    const startAttr = ordered && items[0].number !== 1 ? ` start="${items[0].number}"` : '';
    const html = items.map(({ body }) => {
      let task = '';
      const check = /^\[([ xX])\][ \t]+/.exec(body[0] || '');
      if (check) {
        body[0] = body[0].slice(check[0].length);
        task = `<input type="checkbox" disabled${check[1] === ' ' ? '' : ' checked'}> `;
      }
      return `<li${task ? ' class="task"' : ''}>${task}${renderBlocks(body, ctx, !loose)}</li>`;
    });
    out.push(`<${tag}${startAttr}>${html.join('')}</${tag}>`);
    return i;
  }

  function renderMarkdown(src, opts) {
    const options = opts || {};
    const lines = String(src || '').replace(/\r\n?/g, '\n').split('\n').map(expandTabs);
    // Reference definitions are read first, so a link may come before its definition.
    const refs = {};
    const kept = [];
    let inFence = false;
    for (const line of lines) {
      if (FENCE.test(line)) inFence = !inFence;
      const def = !inFence && DEFINITION.exec(line);
      if (def) {
        const key = normalizeLabel(def[1]);
        if (!refs[key]) refs[key] = { dest: def[2], title: def[3] || def[4] || def[5] || null };
      } else {
        kept.push(line);
      }
    }
    return renderBlocks(kept, { opts: options, refs }, false);
  }

  // The text of some markdown with its marks taken out, for search.
  function plainText(src) {
    return String(src || '')
      .replace(/```[\s\S]*?```/g, ' ')
      .replace(/!?\[([^\]]*)\]\([^)]*\)/g, '$1')
      .replace(/[`*_~#>|]/g, ' ')
      .replace(/\s+/g, ' ')
      .trim();
  }

  const api = { renderMarkdown, renderInline: (src, opts) => renderInline(src, opts || {}, {}, false), escapeHtml, highlight, parseCodeTarget, plainText };
  if (typeof module === 'object' && module.exports) module.exports = api;
  else root.CrystalMarkdown = api;
})(typeof self !== 'undefined' ? self : this);
