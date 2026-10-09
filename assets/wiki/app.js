// The wiki's page: reads a wiki.json (fetched beside the page under `crystal wiki serve`, or inlined into an
// exported page) and lays it out as Code Wiki does: the outline on the left following the reader down, the
// document in the middle with a diagram card over each part, and the chat on the right, which asks
// `crystal wiki serve` and reads its answer as it streams in. Diagrams are drawn by mermaid, loaded once the
// first card comes near and drawn one at a time as each comes near. Everything is asked for relative to the
// page (assets/, wiki.json, api/), so the same page works served at /p/<key>/ and exported.
(() => {
  'use strict';

  const md = window.CrystalMarkdown;
  const esc = md.escapeHtml;
  const $ = (id) => document.getElementById(`cw-${id}`);
  const MERMAID_URL = new URL('mermaid.min.js', document.currentScript.src).href;
  const THEME_KEY = 'crystal-wiki-theme';
  const CHAT_KEY = 'crystal-wiki-chat';
  const WIDE = 1180;
  const PHONE = 840;

  const store = {
    get(key) {
      try {
        return localStorage.getItem(key);
      } catch (_) {
        return null;
      }
    },
    set(key, value) {
      try {
        localStorage.setItem(key, value);
      } catch (_) {
        // A private window, or storage turned off: the choice lasts as long as the page.
      }
    },
  };

  const S = {
    mode: 'serve', // 'serve' (crystal wiki serve) or 'static' (an export, the wiki inlined)
    base: '', // the page's own path, which wiki.json and api/ are under
    wiki: null,
    entries: [], // every heading the outline lists: { id, title, level, section, el, link }
    byId: new Map(),
    active: -1,
    diagrams: [],
    projects: null,
  };

  // ---- Small helpers ----------------------------------------------------------------------------------

  const shortSha = (sha) => String(sha || '').slice(0, 7);
  const reduceMotion = () => matchMedia('(prefers-reduced-motion: reduce)').matches;
  const isPhone = () => innerWidth < PHONE;
  const isWide = () => innerWidth >= WIDE;

  function formatDate(at) {
    const date = new Date(at);
    if (Number.isNaN(date.getTime())) return '';
    return date.toLocaleDateString(undefined, { year: 'numeric', month: 'short', day: 'numeric' });
  }

  let toastTimer = 0;
  function toast(text) {
    const el = $('toast');
    el.textContent = text;
    el.hidden = false;
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => {
      el.hidden = true;
    }, 2600);
  }

  async function copyText(text) {
    try {
      await navigator.clipboard.writeText(text);
      return true;
    } catch (_) {
      // Not a secure context, or no permission: the old way.
    }
    const area = document.createElement('textarea');
    area.value = text;
    area.setAttribute('readonly', '');
    area.style.cssText = 'position:fixed;top:0;left:0;opacity:0';
    document.body.appendChild(area);
    area.select();
    let ok = false;
    try {
      ok = document.execCommand('copy');
    } catch (_) {
      ok = false;
    }
    area.remove();
    return ok;
  }

  const idle = () =>
    new Promise((resolve) => {
      if (window.requestIdleCallback) requestIdleCallback(() => resolve(), { timeout: 120 });
      else setTimeout(resolve, 16);
    });

  // ---- The theme ----------------------------------------------------------------------------------------

  const darkQuery = matchMedia('(prefers-color-scheme: dark)');
  const themePref = () => {
    const pref = store.get(THEME_KEY);
    return pref === 'dark' || pref === 'light' ? pref : 'system';
  };
  const themeNow = () => {
    const pref = themePref();
    return pref === 'system' ? (darkQuery.matches ? 'dark' : 'light') : pref;
  };
  const THEME_ICONS = { dark: 'moon', light: 'sun', system: 'contrast' };

  function applyTheme(pref) {
    const root = document.documentElement;
    if (pref === 'system') delete root.dataset.theme;
    else root.dataset.theme = pref;
    $('theme-btn').querySelector('use').setAttribute('href', `#cw-i-${THEME_ICONS[pref]}`);
    $('theme-btn').title = `Theme: ${pref}`;
    for (const item of $('theme-menu').querySelectorAll('[data-theme]')) item.setAttribute('aria-checked', String(item.dataset.theme === pref));
    if (window.mermaid && D.theme !== themeNow()) initMermaid();
  }

  function setTheme(pref) {
    store.set(THEME_KEY, pref);
    applyTheme(pref);
  }

  // ---- Loading ------------------------------------------------------------------------------------------

  async function load() {
    const inline = document.getElementById('wiki-data');
    if (inline) {
      S.mode = 'static';
      return JSON.parse(inline.textContent);
    }
    if (window.CRYSTAL_WIKI) {
      S.mode = 'static';
      return window.CRYSTAL_WIKI;
    }
    const served = /^(.*\/p\/[^/]+\/)/.exec(location.pathname);
    if (served) {
      S.mode = 'serve';
      S.base = served[1];
      const res = await fetch(`${S.base}wiki.json`, { cache: 'no-cache' });
      if (!res.ok) throw new Error(res.status === 404 ? 'there is no wiki here' : `crystal answered ${res.status}`);
      return res.json();
    }
    throw new Error('this page has no wiki in it');
  }

  // ---- Code links -----------------------------------------------------------------------------------------

  function codeUrl(path, line, end) {
    const repo = S.wiki && S.wiki.repo;
    if (!repo || !repo.code_url) return null;
    const encoded = path.split('/').map(encodeURIComponent).join('/');
    let url = repo.code_url.replace('{commit}', encodeURIComponent(repo.commit || 'HEAD')).replace('{path}', encoded);
    if (line) url += `#L${line}${end ? `-L${end}` : ''}`;
    return url;
  }

  function codeTarget(path, line, end) {
    const where = `${path}${line ? `:${line}` : ''}`;
    const href = codeUrl(path, line, end);
    if (S.mode === 'serve') {
      const forge = href ? `; ${navigator.platform.startsWith('Mac') ? '⌘' : 'Ctrl'}-click opens it on the forge` : '';
      return { href, title: `Open ${where} in your editor${forge}` };
    }
    return href ? { href, title: `${where} at ${shortSha(S.wiki.repo.commit)}` } : null;
  }

  const mdOptions = (offset) => ({ code: codeTarget, headingOffset: offset });

  async function openInEditor(link) {
    const path = link.dataset.path;
    const line = link.dataset.line;
    const query = `path=${encodeURIComponent(path)}${line ? `&line=${line}` : ''}`;
    try {
      const res = await fetch(`${S.base}api/open?${query}`);
      if (res.ok) toast(`Opened ${path}${line ? `:${line}` : ''} in your editor`);
      else if (res.status === 404) toast(`${path} isn't in the repository`);
      else toast(`Couldn't open ${path}: ${(await res.text()).trim() || res.status}`);
    } catch (err) {
      toast(`Couldn't reach crystal to open ${path}`);
    }
  }

  // A click on a link into the code opens it in the editor under `crystal wiki serve`; with ⌘ or Ctrl (or
  // the middle button, which never gets here) the browser follows it to the forge.
  function onCodeLink(event) {
    const link = event.target.closest('a.code-link');
    if (!link || S.mode !== 'serve') return;
    if (event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) {
      if (!link.hasAttribute('href')) event.preventDefault();
      return;
    }
    event.preventDefault();
    openInEditor(link);
  }

  // ---- The document ---------------------------------------------------------------------------------------

  function card(diagram, label) {
    if (!diagram || typeof diagram.mermaid !== 'string' || !diagram.mermaid.trim()) return '';
    const caption = diagram.caption || label || 'Diagram';
    const i = S.diagrams.push({ src: diagram.mermaid, caption, state: 'idle' }) - 1;
    return cardHtml(i, caption);
  }

  function cardHtml(i, caption) {
    return (
      `<figure class="diagram-card" data-diagram="${i}" aria-label="${esc(caption)}">` +
      '<div class="diagram-view"><div class="spinner"></div></div>' +
      '<button class="zoom-btn" type="button" aria-label="Zoom into the diagram" title="Zoom"><svg class="icon"><use href="#cw-i-zoom"/></svg></button>' +
      `<figcaption class="caption-sr">${esc(caption)}</figcaption></figure>`
    );
  }

  function headingHtml(level, id, title) {
    return (
      `<h${level} class="heading" id="${esc(id)}"><span>${esc(title)}</span>` +
      `<button class="anchor" type="button" data-anchor="${esc(id)}" aria-label="Copy a link to “${esc(title)}”" title="Copy link">` +
      '<svg class="icon"><use href="#cw-i-link"/></svg></button></h' +
      `${level}>`
    );
  }

  // Ids are the page's anchors; a wiki that repeats one has the repeat numbered rather than lost.
  function uniqueId(id, title, seen) {
    let base = String(id || title || 'section')
      .trim()
      .replace(/\s+/g, '-');
    if (!base) base = 'section';
    let out = base;
    for (let n = 2; seen.has(out) || document.getElementById(out); n++) out = `${base}-${n}`;
    seen.add(out);
    return out;
  }

  function renderWiki(wiki) {
    const repo = wiki.repo || {};
    const generated = wiki.generated || {};
    const name = repo.name || 'Repository';
    document.title = `${name} · crystal wiki`;
    $('title').textContent = name;

    const badge = $('badge');
    badge.hidden = false;
    const made = [generated.by, generated.at && formatDate(generated.at), typeof generated.cost_usd === 'number' ? `$${generated.cost_usd.toFixed(2)}` : null].filter(Boolean);
    badge.title = made.length ? `Written by ${made.join(' · ')}` : 'Written with Claude';
    $('help-made').textContent = `crystal wrote this page about ${name} from its code${generated.by ? ` with ${generated.by}` : ', with Claude'}${generated.at ? ` on ${formatDate(generated.at)}` : ''}, at commit ${shortSha(repo.commit)}${generated.crystal ? ` (crystal ${generated.crystal})` : ''}.`;
    $('ai-note').textContent = `${generated.by || 'AI'} can make mistakes, so double-check it.`;

    if (repo.web_url) {
      const link = $('repo-link');
      const host = /gitlab/i.test(repo.web_url) ? 'GitLab' : /github\.com/i.test(repo.web_url) ? 'GitHub' : 'the forge';
      link.href = repo.web_url;
      link.hidden = false;
      link.setAttribute('aria-label', `${name} on ${host}`);
      link.title = `${name} on ${host}`;
      link.innerHTML = `<svg class="icon"><use href="#cw-i-${host === 'GitLab' ? 'gitlab' : host === 'GitHub' ? 'github' : 'open'}"/></svg>`;
    }

    $('updated').textContent = formatDate(generated.at) || '—';
    const commitUrl = repo.web_url ? (/gitlab/i.test(repo.web_url) ? `${repo.web_url}/-/tree/${repo.commit}` : `${repo.web_url}/tree/${repo.commit}`) : null;
    $('commit').innerHTML = commitUrl
      ? `<a href="${esc(commitUrl)}" target="_blank" rel="noopener noreferrer" title="${esc(repo.commit || '')}">${esc(shortSha(repo.commit))}</a>`
      : `<span title="${esc(repo.commit || '')}">${esc(shortSha(repo.commit) || '—')}</span>`;

    const seen = new Set();
    let html = '';
    if (wiki.version !== 1) {
      html += `<p class="notice">This wiki says it's version ${esc(String(wiki.version))}, which this page doesn't know; some of it may not show.</p>`;
    }
    const overview = wiki.overview || {};
    const overviewCard = card(overview.diagram, `${name} at a glance`);
    html += `<section class="overview" aria-label="Overview"><div class="overview-grid${overviewCard ? '' : ' no-diagram'}">${overviewCard}<div class="prose">${md.renderMarkdown(overview.summary_md || '', mdOptions(2))}</div></div></section>`;

    S.entries = [];
    for (const section of wiki.sections || []) {
      const id = uniqueId(section.id, section.title, seen);
      const title = section.title || id;
      const index = S.entries.push({ id, title, level: 2, section: -1 }) - 1;
      S.entries[index].section = index;
      html += `<section class="sec">${headingHtml(2, id, title)}${card(section.diagram, title)}<div class="prose">${md.renderMarkdown(section.summary_md || '', mdOptions(2))}</div>`;
      for (const sub of section.subsections || []) {
        const subId = uniqueId(sub.id, sub.title, seen);
        const subTitle = sub.title || subId;
        S.entries.push({ id: subId, title: subTitle, level: 3, section: index, files: sub.files || [] });
        html += `<section class="sub">${headingHtml(3, subId, subTitle)}${card(sub.diagram, subTitle)}<div class="prose">${md.renderMarkdown(sub.body_md || '', mdOptions(3))}</div></section>`;
      }
      html += '</section>';
    }
    const body = $('doc-body');
    body.innerHTML = html;
    for (const fence of body.querySelectorAll('figure[data-mermaid]')) hydrateFence(fence);

    S.byId = new Map();
    S.entries.forEach((entry, i) => {
      entry.el = document.getElementById(entry.id);
      S.byId.set(entry.id, i);
    });
    renderOutline();
    indexForFind(wiki);
    watchHeadings();
    watchCards(body);
  }

  // A ```mermaid fence in the text becomes a card like the others.
  function hydrateFence(fence) {
    const src = fence.querySelector('.mermaid-src').textContent;
    const i = S.diagrams.push({ src, caption: 'Diagram', state: 'idle' }) - 1;
    fence.insertAdjacentHTML('afterend', cardHtml(i, 'Diagram'));
    fence.remove();
  }

  // ---- The outline ----------------------------------------------------------------------------------------

  function renderOutline() {
    let html = '';
    S.entries.forEach((entry, i) => {
      if (entry.level !== 2) return;
      const subs = [];
      for (let j = i + 1; j < S.entries.length && S.entries[j].level === 3; j++) subs.push(S.entries[j]);
      const list = subs.length ? `<ol>${subs.map((sub) => `<li><a href="#${esc(sub.id)}" data-id="${esc(sub.id)}">${esc(sub.title)}</a></li>`).join('')}</ol>` : '';
      html += `<li data-id="${esc(entry.id)}"><a href="#${esc(entry.id)}" data-id="${esc(entry.id)}">${esc(entry.title)}</a>${list}</li>`;
    });
    const list = $('outline-list');
    list.innerHTML = html;
    for (const link of list.querySelectorAll('a[data-id]')) {
      const entry = S.entries[S.byId.get(link.dataset.id)];
      if (entry) entry.link = link;
    }
  }

  let outlineLockUntil = 0;

  function setActive(index, follow = true) {
    if (index === S.active || index < 0 || index >= S.entries.length) return;
    const before = S.entries[S.active];
    const now = S.entries[index];
    if (before && before.link) before.link.classList.remove('active');
    if (now.link) now.link.classList.add('active');
    const beforeSection = before ? S.entries[before.section] : null;
    const nowSection = S.entries[now.section];
    if (beforeSection !== nowSection) {
      if (beforeSection && beforeSection.link) beforeSection.link.parentElement.classList.remove('open');
      if (nowSection && nowSection.link) nowSection.link.parentElement.classList.add('open');
    }
    S.active = index;
    if (follow && now.link) keepInView($('outline-list'), now.level === 3 ? now.link : nowSection.link.parentElement);
  }

  // Scrolls the outline, and only the outline, so the entry is in sight.
  function keepInView(list, el) {
    const top = el.offsetTop - list.offsetTop;
    const bottom = top + el.offsetHeight;
    if (top < list.scrollTop + 8) list.scrollTop = Math.max(0, top - 8);
    else if (bottom > list.scrollTop + list.clientHeight - 8) list.scrollTop = bottom - list.clientHeight + 8;
  }

  let headingObserver = null;
  const visible = new Set();

  // The entry in view is the topmost heading in a band at the top of the window; with none in it, the one
  // last scrolled past. At the bottom of the page, where the last headings can't reach the band, the last
  // heading in sight.
  function watchHeadings() {
    if (headingObserver) headingObserver.disconnect();
    visible.clear();
    const header = document.querySelector('.top').offsetHeight;
    headingObserver = new IntersectionObserver(onHeadings, { rootMargin: `-${header}px 0px -62% 0px` });
    for (const entry of S.entries) if (entry.el) headingObserver.observe(entry.el);
  }

  function onHeadings(records) {
    let above = Infinity;
    for (const record of records) {
      const index = S.byId.get(record.target.id);
      if (record.isIntersecting) visible.add(index);
      else {
        visible.delete(index);
        if (record.rootBounds && record.boundingClientRect.top >= record.rootBounds.bottom) above = Math.min(above, index - 1);
      }
    }
    if (performance.now() < outlineLockUntil) return;
    if (visible.size) setActive(Math.min(...visible));
    else if (above !== Infinity) setActive(Math.max(0, above));
    else if (S.active < 0) setActive(0);
  }

  let scrollQueued = false;
  function onScroll() {
    if (scrollQueued) return;
    scrollQueued = true;
    requestAnimationFrame(() => {
      scrollQueued = false;
      const doc = document.documentElement;
      if (S.entries.length && innerHeight + scrollY >= doc.scrollHeight - 4 && performance.now() >= outlineLockUntil) {
        for (let i = S.entries.length - 1; i >= 0; i--) {
          const el = S.entries[i].el;
          if (el && el.getBoundingClientRect().top < innerHeight * 0.85) {
            setActive(i);
            break;
          }
        }
      }
    });
  }

  function goTo(id, push) {
    const index = S.byId.get(id);
    const el = document.getElementById(id);
    if (!el) return;
    if (index !== undefined) {
      setActive(index);
      outlineLockUntil = performance.now() + (reduceMotion() ? 100 : 900);
    }
    el.scrollIntoView({ behavior: reduceMotion() ? 'auto' : 'smooth', block: 'start' });
    history[push ? 'pushState' : 'replaceState'](null, '', `#${encodeURIComponent(id)}`);
  }

  function step(delta) {
    const from = S.active < 0 ? -1 : S.active;
    const to = Math.max(0, Math.min(S.entries.length - 1, from + delta));
    if (S.entries[to]) goTo(S.entries[to].id, false);
  }

  // ---- Diagrams -----------------------------------------------------------------------------------------

  const D = { loading: null, queue: [], running: false, theme: null, io: null, failed: null };

  function watchCards(root) {
    if (!D.io) {
      D.io = new IntersectionObserver(
        (records) => {
          for (const record of records) {
            const d = S.diagrams[Number(record.target.dataset.diagram)];
            if (!d) continue;
            d.near = record.isIntersecting;
            if (d.near && (d.state === 'idle' || d.state === 'queued')) enqueue(d);
          }
        },
        { rootMargin: '700px 0px' },
      );
    }
    for (const el of root.querySelectorAll('.diagram-card[data-diagram]')) {
      const d = S.diagrams[Number(el.dataset.diagram)];
      if (!d || d.card) continue;
      d.card = el;
      D.io.observe(el);
    }
  }

  function enqueue(d) {
    if (d.state === 'queued' && D.queue.includes(d)) return;
    d.state = 'queued';
    D.queue.push(d);
    pump();
  }

  // Draws the queued diagrams one at a time, the nearest first, letting the page breathe between them.
  async function pump() {
    if (D.running) return;
    D.running = true;
    try {
      await loadMermaid();
      while (D.queue.length) {
        const mid = innerHeight / 2;
        const distance = (d) => {
          const rect = d.card.getBoundingClientRect();
          return Math.abs(rect.top + rect.height / 2 - mid);
        };
        D.queue.sort((a, b) => distance(a) - distance(b));
        const d = D.queue.shift();
        if (!d.near && !d.force) {
          d.state = 'idle';
          continue;
        }
        await draw(d);
        await idle();
      }
    } catch (err) {
      D.failed = err.message;
      for (const d of D.queue.splice(0)) fail(d, `Diagrams need mermaid, which didn't load: ${err.message}.`);
    } finally {
      D.running = false;
    }
  }

  function loadMermaid() {
    if (D.loading) return D.loading;
    D.loading = (async () => {
      if (!window.mermaid) {
        await new Promise((resolve, reject) => {
          const script = document.createElement('script');
          script.src = MERMAID_URL;
          script.async = true;
          script.onload = resolve;
          script.onerror = () => reject(new Error(`couldn't load ${MERMAID_URL}`));
          document.head.appendChild(script);
        });
      }
      if (!window.mermaid) throw new Error('mermaid.min.js loaded but defined no mermaid');
      try {
        await document.fonts.load('12px "Google Sans Code"');
      } catch (_) {
        // Drawn in the fallback font.
      }
      initMermaid();
      performance.mark('cw-mermaid');
    })();
    return D.loading;
  }

  // Mermaid's own colours for the theme in force; the page's stylesheet colours what it draws from the
  // theme's variables over them, so the diagrams follow a change of theme without being drawn again.
  function initMermaid() {
    const styles = getComputedStyle(document.documentElement);
    const v = (name) => styles.getPropertyValue(name).trim();
    const dark = themeNow() === 'dark';
    const c = { card: v('--card') || '#303030', line: v('--dg-line') || '#e3e3e3', text: v('--dg-text') || '#fff', faint: v('--dg-faint') || '#8e918f', panel: v('--panel') || '#131314' };
    const font = '"Google Sans Code", ui-monospace, SFMono-Regular, Menlo, monospace';
    window.mermaid.initialize({
      startOnLoad: false,
      securityLevel: 'strict',
      theme: 'base',
      darkMode: dark,
      fontFamily: font,
      fontSize: 12,
      themeVariables: {
        darkMode: dark,
        fontFamily: font,
        fontSize: '12px',
        background: c.card,
        mainBkg: c.card,
        primaryColor: c.card,
        primaryTextColor: c.text,
        primaryBorderColor: c.line,
        secondaryColor: c.card,
        secondaryTextColor: c.text,
        secondaryBorderColor: c.line,
        tertiaryColor: c.card,
        tertiaryTextColor: c.text,
        tertiaryBorderColor: c.line,
        nodeBorder: c.line,
        nodeTextColor: c.text,
        lineColor: c.line,
        textColor: c.text,
        titleColor: c.text,
        clusterBkg: c.card,
        clusterBorder: c.faint,
        edgeLabelBackground: c.card,
        actorBkg: c.card,
        actorBorder: c.line,
        actorTextColor: c.text,
        actorLineColor: c.faint,
        signalColor: c.line,
        signalTextColor: c.text,
        labelBoxBkgColor: c.card,
        labelBoxBorderColor: c.line,
        labelTextColor: c.text,
        loopTextColor: c.text,
        noteBkgColor: c.card,
        noteBorderColor: c.faint,
        noteTextColor: c.text,
        activationBkgColor: c.panel,
        activationBorderColor: c.line,
      },
      flowchart: { curve: 'linear', htmlLabels: true, padding: 10, nodeSpacing: 44, rankSpacing: 62, diagramPadding: 10, useMaxWidth: false },
      sequence: { useMaxWidth: false, mirrorActors: false, actorFontFamily: font, noteFontFamily: font, messageFontFamily: font, actorFontSize: 12, noteFontSize: 12, messageFontSize: 12, boxMargin: 8 },
      class: { useMaxWidth: false },
      state: { useMaxWidth: false },
      er: { useMaxWidth: false },
      gantt: { useMaxWidth: false },
      journey: { useMaxWidth: false },
      mindmap: { useMaxWidth: false },
      timeline: { useMaxWidth: false },
    });
    D.theme = themeNow();
  }

  let drawn = 0;
  async function draw(d) {
    const i = S.diagrams.indexOf(d);
    const id = `cw-mmd-${i}-${drawn++}`;
    d.state = 'drawing';
    try {
      const { svg } = await window.mermaid.render(id, d.src);
      d.id = id;
      d.svg = svg;
      const view = d.card.querySelector('.diagram-view');
      view.innerHTML = svg;
      const el = view.querySelector('svg');
      const box = el.viewBox && el.viewBox.baseVal;
      d.w = (box && box.width) || parseFloat(el.getAttribute('width')) || 600;
      d.h = (box && box.height) || parseFloat(el.getAttribute('height')) || 400;
      el.removeAttribute('width');
      el.removeAttribute('height');
      el.setAttribute('style', `max-width:${d.w}px;max-height:${d.h}px`);
      el.setAttribute('preserveAspectRatio', 'xMidYMid meet');
      el.setAttribute('aria-hidden', 'true');
      d.state = 'drawn';
      d.card.classList.add('drawn');
    } catch (err) {
      // Mermaid leaves its drawing of the error behind; the source is shown in its place.
      for (const leftover of [document.getElementById(`d${id}`), document.getElementById(id)]) {
        if (leftover && !d.card.contains(leftover)) leftover.remove();
      }
      const why = err && err.message ? String(err.message).split('\n')[0].replace(/[\s:.]+$/, '') : '';
      fail(d, `This diagram couldn't be drawn${why ? `: ${why}` : ''}.`);
    }
  }

  function fail(d, why) {
    d.state = 'failed';
    d.card.classList.add('failed');
    d.card.querySelector('.diagram-view').innerHTML = `<p class="diagram-fail">${esc(why)}</p><pre class="code-block"><code>${esc(d.src)}</code></pre>`;
  }

  // ---- The zoomed diagram ---------------------------------------------------------------------------------

  const Z = { x: 0, y: 0, k: 1, w: 0, h: 0, pointers: new Map(), pinch: null };

  function openZoom(d) {
    if (!d || d.state !== 'drawn') return;
    const content = $('zoom-content');
    // A copy of its own, its ids renamed so its markers and styles don't meet the card's.
    content.innerHTML = d.svg.split(d.id).join(`${d.id}-zoom`);
    const svg = content.querySelector('svg');
    svg.setAttribute('width', d.w);
    svg.setAttribute('height', d.h);
    svg.setAttribute('style', 'max-width:none');
    Z.w = d.w;
    Z.h = d.h;
    $('zoom-caption').textContent = d.caption || '';
    $('zoom').showModal();
    fitZoom();
  }

  function applyZoom() {
    $('zoom-content').style.transform = `translate(${Z.x}px, ${Z.y}px) scale(${Z.k})`;
  }

  function fitZoom() {
    const stage = $('zoom-stage').getBoundingClientRect();
    const top = isPhone() ? 64 : 84;
    const k = Math.min((stage.width - 48) / Z.w, (stage.height - top - 32) / Z.h, 2.5);
    Z.k = Math.max(0.05, k);
    Z.x = (stage.width - Z.w * Z.k) / 2;
    Z.y = top + (stage.height - top - 32 - Z.h * Z.k) / 2;
    applyZoom();
  }

  function zoomAt(factor, cx, cy) {
    const k = Math.min(10, Math.max(0.05, Z.k * factor));
    Z.x = cx - (cx - Z.x) * (k / Z.k);
    Z.y = cy - (cy - Z.y) * (k / Z.k);
    Z.k = k;
    applyZoom();
  }

  function zoomCentre(factor) {
    const stage = $('zoom-stage').getBoundingClientRect();
    zoomAt(factor, stage.width / 2, stage.height / 2);
  }

  function wireZoom() {
    const dialog = $('zoom');
    const stage = $('zoom-stage');
    $('zoom-close').addEventListener('click', () => dialog.close());
    $('zoom-in').addEventListener('click', () => zoomCentre(1.3));
    $('zoom-out').addEventListener('click', () => zoomCentre(1 / 1.3));
    $('zoom-fit').addEventListener('click', fitZoom);
    dialog.addEventListener('close', () => {
      $('zoom-content').innerHTML = '';
      Z.pointers.clear();
    });
    dialog.addEventListener('keydown', (event) => {
      if (event.key === '+' || event.key === '=') zoomCentre(1.3);
      else if (event.key === '-' || event.key === '_') zoomCentre(1 / 1.3);
      else if (event.key === '0') fitZoom();
      else if (event.key.startsWith('Arrow')) {
        const d = 60;
        if (event.key === 'ArrowLeft') Z.x += d;
        if (event.key === 'ArrowRight') Z.x -= d;
        if (event.key === 'ArrowUp') Z.y += d;
        if (event.key === 'ArrowDown') Z.y -= d;
        applyZoom();
      } else return;
      event.preventDefault();
    });
    stage.addEventListener(
      'wheel',
      (event) => {
        event.preventDefault();
        const rect = stage.getBoundingClientRect();
        if (event.ctrlKey || event.metaKey) {
          const factor = Math.min(1.25, Math.max(0.8, Math.exp(-event.deltaY * (event.deltaMode ? 0.05 : 0.01))));
          zoomAt(factor, event.clientX - rect.left, event.clientY - rect.top);
        } else {
          const unit = event.deltaMode === 1 ? 16 : event.deltaMode === 2 ? rect.height : 1;
          Z.x -= event.deltaX * unit;
          Z.y -= event.deltaY * unit;
          applyZoom();
        }
      },
      { passive: false },
    );
    stage.addEventListener('dblclick', (event) => {
      const rect = stage.getBoundingClientRect();
      zoomAt(event.shiftKey ? 1 / 1.6 : 1.6, event.clientX - rect.left, event.clientY - rect.top);
    });
    stage.addEventListener('pointerdown', (event) => {
      stage.setPointerCapture(event.pointerId);
      Z.pointers.set(event.pointerId, { x: event.clientX, y: event.clientY });
      stage.classList.add('dragging');
      if (Z.pointers.size === 2) {
        const [a, b] = [...Z.pointers.values()];
        Z.pinch = { d: Math.hypot(a.x - b.x, a.y - b.y), k: Z.k };
      }
    });
    stage.addEventListener('pointermove', (event) => {
      const last = Z.pointers.get(event.pointerId);
      if (!last) return;
      const now = { x: event.clientX, y: event.clientY };
      Z.pointers.set(event.pointerId, now);
      if (Z.pointers.size === 1) {
        Z.x += now.x - last.x;
        Z.y += now.y - last.y;
        applyZoom();
      } else if (Z.pointers.size === 2 && Z.pinch) {
        const [a, b] = [...Z.pointers.values()];
        const rect = stage.getBoundingClientRect();
        const d = Math.hypot(a.x - b.x, a.y - b.y);
        zoomAt((Z.pinch.k * (d / Z.pinch.d)) / Z.k, (a.x + b.x) / 2 - rect.left, (a.y + b.y) / 2 - rect.top);
      }
    });
    const release = (event) => {
      Z.pointers.delete(event.pointerId);
      if (Z.pointers.size < 2) Z.pinch = null;
      if (!Z.pointers.size) stage.classList.remove('dragging');
    };
    stage.addEventListener('pointerup', release);
    stage.addEventListener('pointercancel', release);
    window.addEventListener('resize', () => {
      if (dialog.open) fitZoom();
    });
  }

  // ---- Find ---------------------------------------------------------------------------------------------

  const F = { items: [], sel: -1, shown: [] };

  function indexForFind(wiki) {
    F.items = [];
    let index = 0;
    for (const section of wiki.sections || []) {
      const entry = S.entries[index];
      F.items.push({ id: entry.id, title: entry.title, context: '', text: md.plainText(section.summary_md).toLowerCase(), raw: md.plainText(section.summary_md), files: '' });
      index++;
      for (const sub of section.subsections || []) {
        const subEntry = S.entries[index];
        F.items.push({ id: subEntry.id, title: subEntry.title, context: entry.title, text: md.plainText(sub.body_md).toLowerCase(), raw: md.plainText(sub.body_md), files: (sub.files || []).join(' ').toLowerCase() });
        index++;
      }
    }
  }

  // The other wikis the server has, for Find: `/projects.json`, two levels up from `/p/<key>/`.
  async function fetchProjects() {
    if (S.projects || S.mode !== 'serve') return S.projects || [];
    try {
      const res = await fetch(new URL('../../projects.json', location.href), { cache: 'no-cache' });
      if (!res.ok || !/json/.test(res.headers.get('content-type') || '')) throw new Error(String(res.status));
      const data = await res.json();
      const list = Array.isArray(data) ? data : (data && data.projects) || [];
      S.projects = list.filter((p) => p && p.key && p.name);
    } catch (_) {
      S.projects = [];
    }
    return S.projects;
  }

  function highlightWords(text, words) {
    let html = esc(text);
    for (const word of words) {
      if (!word) continue;
      const re = new RegExp(`(${esc(word).replace(/[.*+?^${}()|[\]\\]/g, '\\$&')})`, 'gi');
      html = html.replace(re, '<mark>$1</mark>');
    }
    return html;
  }

  function snippet(raw, word) {
    const at = raw.toLowerCase().indexOf(word);
    if (at < 0) return raw.slice(0, 90);
    const from = Math.max(0, at - 36);
    return `${from ? '…' : ''}${raw.slice(from, at + 70)}…`;
  }

  function findResults(query) {
    const words = query.toLowerCase().split(/\s+/).filter(Boolean);
    if (!words.length) return { page: [], repos: [] };
    const scored = [];
    for (const item of F.items) {
      const title = item.title.toLowerCase();
      let score = 0;
      if (words.every((w) => title.includes(w))) score = 3 + (title.startsWith(words[0]) ? 1 : 0);
      else if (words.every((w) => title.includes(w) || item.files.includes(w))) score = 2;
      else if (words.every((w) => item.text.includes(w) || title.includes(w))) score = 1;
      if (score) scored.push({ item, score });
    }
    scored.sort((a, b) => b.score - a.score);
    const page = scored.slice(0, 8).map(({ item, score }) => ({
      kind: 'section',
      id: item.id,
      title: highlightWords(item.title, words),
      context: score === 1 ? esc(snippet(item.raw, words[0])) : esc(item.context || 'Section'),
    }));
    const repos = (S.projects || [])
      .filter((p) => {
        const hay = `${p.name} ${p.root || ''}`.toLowerCase();
        return words.every((w) => hay.includes(w));
      })
      .slice(0, 6)
      .map((p) => ({ kind: 'repo', href: p.url || `../../p/${encodeURIComponent(p.key)}/`, title: highlightWords(p.name, words), context: esc([p.root, p.updated && `updated ${formatDate(p.updated)}`].filter(Boolean).join(' · ')) }));
    return { page, repos };
  }

  function renderFind() {
    const input = $('find');
    const box = $('find-results');
    const query = input.value.trim();
    if (!query) {
      box.hidden = true;
      input.setAttribute('aria-expanded', 'false');
      F.shown = [];
      return;
    }
    const { page, repos } = findResults(query);
    F.shown = [...page, ...repos];
    F.sel = F.shown.length ? 0 : -1;
    let html = '';
    let n = 0;
    const item = (r) => {
      const attrs = r.kind === 'repo' ? `href="${esc(r.href)}"` : `href="#${esc(r.id)}" data-id="${esc(r.id)}"`;
      return `<a class="find-item${n === F.sel ? ' sel' : ''}" role="option" id="cw-find-${n++}" ${attrs}><span class="find-title">${r.title}</span><span class="find-context">${r.context}</span></a>`;
    };
    if (page.length) html += `<div class="find-group">On this page</div>${page.map(item).join('')}`;
    if (repos.length) html += `<div class="find-group">Repositories</div>${repos.map(item).join('')}`;
    if (!html) html = `<div class="find-none">Nothing found for “${esc(query)}”</div>`;
    box.innerHTML = html;
    box.hidden = false;
    input.setAttribute('aria-expanded', 'true');
    input.setAttribute('aria-activedescendant', F.sel >= 0 ? 'cw-find-0' : '');
  }

  function moveFind(delta) {
    if (!F.shown.length) return;
    const items = $('find-results').querySelectorAll('.find-item');
    if (items[F.sel]) items[F.sel].classList.remove('sel');
    F.sel = (F.sel + delta + F.shown.length) % F.shown.length;
    items[F.sel].classList.add('sel');
    items[F.sel].scrollIntoView({ block: 'nearest' });
    $('find').setAttribute('aria-activedescendant', items[F.sel].id);
  }

  function closeFind(blur) {
    $('find-results').hidden = true;
    $('find').setAttribute('aria-expanded', 'false');
    document.body.classList.remove('find-open');
    if (blur) $('find').blur();
  }

  function chooseFind(index) {
    const result = F.shown[index];
    if (!result) return;
    if (result.kind === 'repo') {
      location.href = result.href;
      return;
    }
    closeFind(true);
    $('find').value = '';
    goTo(result.id, true);
    closeOutline();
  }

  function wireFind() {
    const input = $('find');
    input.addEventListener('focus', () => {
      fetchProjects().then(() => {
        if (document.activeElement === input && input.value.trim()) renderFind();
      });
      if (input.value.trim()) renderFind();
    });
    input.addEventListener('input', renderFind);
    input.addEventListener('keydown', (event) => {
      if (event.key === 'ArrowDown') moveFind(1);
      else if (event.key === 'ArrowUp') moveFind(-1);
      else if (event.key === 'Enter') chooseFind(F.sel);
      else if (event.key === 'Escape') {
        if (input.value) input.value = '';
        closeFind(true);
      } else return;
      event.preventDefault();
    });
    input.addEventListener('blur', () => {
      setTimeout(() => {
        if (document.activeElement !== input) closeFind(false);
      }, 150);
    });
    $('find-results').addEventListener('mousedown', (event) => event.preventDefault());
    $('find-results').addEventListener('click', (event) => {
      const item = event.target.closest('.find-item');
      if (!item) return;
      event.preventDefault();
      chooseFind([...$('find-results').querySelectorAll('.find-item')].indexOf(item));
    });
    $('find-btn').addEventListener('click', () => {
      document.body.classList.add('find-open');
      input.focus();
    });
  }

  // ---- The chat -------------------------------------------------------------------------------------------

  const C = { conversation: null, busy: false, abort: null };

  function setChat(open, remember = true) {
    document.body.classList.toggle('chat-open', open);
    $('chat-btn').setAttribute('aria-pressed', String(open));
    if (remember && isWide()) store.set(CHAT_KEY, open ? 'open' : 'closed');
    if (open && !isWide() && S.mode === 'serve') setTimeout(() => $('chat-input').focus(), 50);
    // The headings' band moves with the header's height on a narrower window, and the outline with it.
    requestAnimationFrame(() => {
      if (S.wiki) watchHeadings();
    });
  }

  function chatLog() {
    return $('chat-log');
  }

  function nearBottom(el) {
    return el.scrollHeight - el.scrollTop - el.clientHeight < 80;
  }

  function addMessage(cls, html) {
    $('chat-empty').hidden = true;
    $('chat-new').hidden = false;
    const el = document.createElement('div');
    el.className = `msg ${cls}`;
    if (html !== undefined) el.innerHTML = html;
    chatLog().appendChild(el);
    return el;
  }

  function setBusy(busy) {
    C.busy = busy;
    const send = $('chat-send');
    send.querySelector('use').setAttribute('href', busy ? '#cw-i-stop' : '#cw-i-send');
    send.setAttribute('aria-label', busy ? 'Stop' : 'Ask');
    send.title = busy ? 'Stop' : 'Ask';
    updateSend();
  }

  function updateSend() {
    const send = $('chat-send');
    const ready = C.busy || $('chat-input').value.trim().length > 0;
    send.classList.toggle('ready', ready && S.mode === 'serve');
    send.disabled = S.mode !== 'serve' || !ready;
  }

  const TOOL_WORDS = { Read: 'Reading', Grep: 'Searching for', Glob: 'Looking for', LS: 'Listing', Bash: 'Running', WebFetch: 'Fetching' };

  // Reads a text/event-stream body, handing each event's name and its data, parsed, to `on`.
  async function readEvents(body, on) {
    const reader = body.getReader();
    const decoder = new TextDecoder();
    let buffer = '';
    const dispatch = (block) => {
      let name = 'message';
      const data = [];
      for (const line of block.split(/\r\n|\r|\n/)) {
        if (!line || line.startsWith(':')) continue;
        const colon = line.indexOf(':');
        const field = colon < 0 ? line : line.slice(0, colon);
        let value = colon < 0 ? '' : line.slice(colon + 1);
        if (value.startsWith(' ')) value = value.slice(1);
        if (field === 'event') name = value;
        else if (field === 'data') data.push(value);
      }
      if (!data.length) return;
      let parsed;
      try {
        parsed = JSON.parse(data.join('\n'));
      } catch (_) {
        parsed = { text: data.join('\n') };
      }
      on(name, parsed);
    };
    for (;;) {
      const { value, done } = await reader.read();
      if (done) break;
      buffer += decoder.decode(value, { stream: true });
      let match;
      while ((match = /\r\n\r\n|\n\n|\r\r/.exec(buffer))) {
        dispatch(buffer.slice(0, match.index));
        buffer = buffer.slice(match.index + match[0].length);
      }
    }
    buffer += decoder.decode();
    if (buffer.trim()) dispatch(buffer);
  }

  async function errorText(res) {
    const text = (await res.text().catch(() => '')).trim();
    try {
      const json = JSON.parse(text);
      if (json && json.message) return json.message;
    } catch (_) {
      // Plain text.
    }
    return text || `crystal answered ${res.status}`;
  }

  async function ask(question) {
    if (S.mode !== 'serve' || C.busy) return;
    const log = chatLog();
    addMessage('user').textContent = question;
    const bot = addMessage('bot', '<ul class="tools"></ul><div class="answer prose"></div><div class="typing" aria-label="Thinking"><i></i><i></i><i></i></div>');
    const tools = bot.querySelector('.tools');
    const answer = bot.querySelector('.answer');
    const typing = bot.querySelector('.typing');
    log.scrollTop = log.scrollHeight;
    setBusy(true);
    const abort = new AbortController();
    C.abort = abort;
    let text = '';
    let ended = false;
    let queued = false;
    const paint = () => {
      queued = false;
      const stick = nearBottom(log);
      answer.innerHTML = md.renderMarkdown(text, { ...mdOptions(3), diagrams: ended });
      if (stick) log.scrollTop = log.scrollHeight;
    };
    const schedule = () => {
      if (!queued) {
        queued = true;
        requestAnimationFrame(paint);
      }
    };
    const error = (message) => {
      const el = document.createElement('div');
      el.className = 'msg-error';
      el.textContent = message;
      bot.appendChild(el);
    };
    const section = S.entries[S.active] ? S.entries[S.active].id : null;
    try {
      const res = await fetch(`${S.base}api/ask`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json', Accept: 'text/event-stream' },
        body: JSON.stringify({ question, conversation: C.conversation, section }),
        signal: abort.signal,
      });
      if (!res.ok) throw new Error(await errorText(res));
      if (!res.body) throw new Error('the answer came without a stream');
      await readEvents(res.body, (name, data) => {
        if (name === 'delta') {
          typing.hidden = true;
          text += data.text || '';
          schedule();
        } else if (name === 'tool') {
          const li = document.createElement('li');
          const what = data.path
            ? md.renderInline(`[${data.path.replace(/[[\]\\`*_]/g, '\\$&')}](code:${data.path.replace(/[()\s]/g, encodeURIComponent)})`, mdOptions(0))
            : esc(data.pattern || data.command || '');
          li.innerHTML = `<svg class="icon"><use href="#cw-i-file"/></svg><span>${esc(TOOL_WORDS[data.name] || data.name || 'Using a tool')} ${what}</span>`;
          tools.appendChild(li);
          if (nearBottom(log)) log.scrollTop = log.scrollHeight;
        } else if (name === 'done') {
          ended = true;
          if (data.conversation) C.conversation = data.conversation;
          if (typeof data.cost_usd === 'number') bot.dataset.cost = data.cost_usd.toFixed(2);
        } else if (name === 'error') {
          ended = true;
          error(data.message || 'Something went wrong answering that.');
        }
      });
      if (!ended) error(text ? 'The answer stopped before it was done.' : 'crystal closed the stream without an answer.');
    } catch (err) {
      if (err.name === 'AbortError') {
        const note = document.createElement('div');
        note.className = 'meta';
        note.textContent = 'Stopped.';
        bot.appendChild(note);
      } else {
        error(`Couldn't ask: ${err.message || err}`);
      }
    } finally {
      ended = true;
      typing.remove();
      paint();
      if (!text) answer.remove();
      if (!tools.children.length) tools.remove();
      watchCards(answer);
      C.abort = null;
      setBusy(false);
    }
  }

  function wireChat() {
    const input = $('chat-input');
    const form = $('chat-form');
    if (S.mode !== 'serve') {
      input.disabled = true;
      input.placeholder = 'Asking needs crystal wiki serve';
      $('chat-static').hidden = false;
    }
    const grow = () => {
      input.style.height = 'auto';
      input.style.height = `${Math.min(input.scrollHeight, 168)}px`;
      updateSend();
    };
    input.addEventListener('input', grow);
    input.addEventListener('keydown', (event) => {
      if (event.key === 'Enter' && !event.shiftKey && !event.isComposing) {
        event.preventDefault();
        form.requestSubmit();
      }
    });
    form.addEventListener('submit', (event) => {
      event.preventDefault();
      if (C.busy) {
        if (C.abort) C.abort.abort();
        return;
      }
      const question = input.value.trim();
      if (!question) return;
      input.value = '';
      grow();
      ask(question);
    });
    $('chat-new').addEventListener('click', () => {
      if (C.abort) C.abort.abort();
      C.conversation = null;
      for (const msg of chatLog().querySelectorAll('.msg')) msg.remove();
      $('chat-empty').hidden = false;
      $('chat-new').hidden = true;
      input.focus();
    });
    $('chat-close').addEventListener('click', () => setChat(false));
    $('chat-btn').addEventListener('click', () => setChat(!document.body.classList.contains('chat-open')));
    chatLog().addEventListener('click', onCodeLink);
    updateSend();
  }

  // ---- What crystal is doing with the wiki ----------------------------------------------------------------

  async function pollStatus() {
    let next = 30000;
    try {
      const res = await fetch(`${S.base}api/status`, { cache: 'no-store' });
      if (res.ok) {
        const status = await res.json();
        showStatus(status);
        if (status.building) next = 4000;
      }
    } catch (_) {
      // crystal wiki serve has gone; the page stays as it is.
    }
    if (document.visibilityState === 'hidden') document.addEventListener('visibilitychange', () => setTimeout(pollStatus, 500), { once: true });
    else setTimeout(pollStatus, next);
  }

  function showStatus(status) {
    const el = $('status');
    const made = Date.parse((S.wiki.generated || {}).at || '');
    const updated = Date.parse(status.updated || '');
    el.classList.remove('stale');
    if (status.building) {
      el.innerHTML = `<span class="spinner"></span>Updating${status.progress ? ` · ${esc(status.progress)}` : ''}`;
      el.title = 'crystal is bringing this wiki up to date with the code';
    } else if (updated && made && updated > made + 1000) {
      el.innerHTML = 'A newer version is ready · <button type="button">Reload</button>';
      el.title = '';
      el.querySelector('button').addEventListener('click', () => location.reload());
    } else if (status.stale) {
      el.classList.add('stale');
      el.textContent = 'Out of date';
      el.title = 'The default branch has moved on since this was written; crystal wiki update brings it up to date';
    } else {
      el.hidden = true;
      return;
    }
    el.hidden = false;
  }

  // ---- The rest of the page -------------------------------------------------------------------------------

  function openOutline() {
    document.body.classList.add('outline-open');
    $('scrim').hidden = false;
    $('outline-btn').setAttribute('aria-expanded', 'true');
    const active = S.entries[S.active];
    if (active && active.link) requestAnimationFrame(() => keepInView($('outline-list'), active.link));
  }

  function closeOutline() {
    if (!document.body.classList.contains('outline-open')) return;
    document.body.classList.remove('outline-open');
    $('scrim').hidden = true;
    $('outline-btn').setAttribute('aria-expanded', 'false');
  }

  async function share(id) {
    const entry = id ? S.entries[S.byId.get(id)] : S.entries[S.active];
    const url = `${location.href.split('#')[0]}${entry ? `#${encodeURIComponent(entry.id)}` : ''}`;
    if (await copyText(url)) toast(entry ? `Copied a link to “${entry.title}”` : 'Copied a link to this page');
    else toast(url);
  }

  function wirePage() {
    const themeMenu = $('theme-menu');
    $('theme-btn').addEventListener('click', (event) => {
      event.stopPropagation();
      themeMenu.hidden = !themeMenu.hidden;
      $('theme-btn').setAttribute('aria-expanded', String(!themeMenu.hidden));
      if (!themeMenu.hidden) themeMenu.querySelector('[aria-checked="true"]').focus();
    });
    themeMenu.addEventListener('click', (event) => {
      const item = event.target.closest('[data-theme]');
      if (!item) return;
      setTheme(item.dataset.theme);
      themeMenu.hidden = true;
      $('theme-btn').setAttribute('aria-expanded', 'false');
      $('theme-btn').focus();
    });
    document.addEventListener('click', (event) => {
      if (!themeMenu.hidden && !event.target.closest('.menu-wrap')) {
        themeMenu.hidden = true;
        $('theme-btn').setAttribute('aria-expanded', 'false');
      }
    });
    darkQuery.addEventListener('change', () => {
      if (themePref() === 'system') applyTheme('system');
    });
    for (const button of document.querySelectorAll('[data-act="theme"]')) {
      button.addEventListener('click', () => {
        const order = ['dark', 'light', 'system'];
        const next = order[(order.indexOf(themePref()) + 1) % order.length];
        setTheme(next);
        toast(`Theme: ${next}`);
      });
    }

    const help = $('help');
    const openHelp = () => help.showModal();
    $('help-btn').addEventListener('click', openHelp);
    for (const button of document.querySelectorAll('[data-act="help"]')) button.addEventListener('click', openHelp);
    $('help-close').addEventListener('click', () => help.close());
    for (const dialog of [help, $('zoom')]) {
      dialog.addEventListener('click', (event) => {
        if (event.target !== dialog) return;
        const rect = dialog.getBoundingClientRect();
        const inside = event.clientX >= rect.left && event.clientX <= rect.right && event.clientY >= rect.top && event.clientY <= rect.bottom;
        if (!inside) dialog.close();
      });
    }

    $('share-btn').addEventListener('click', () => share(null));
    $('outline-btn').addEventListener('click', () => (document.body.classList.contains('outline-open') ? closeOutline() : openOutline()));
    $('scrim').addEventListener('click', closeOutline);
    $('outline-list').addEventListener('click', (event) => {
      const link = event.target.closest('a[data-id]');
      if (!link || event.metaKey || event.ctrlKey || event.shiftKey) return;
      event.preventDefault();
      goTo(link.dataset.id, true);
      closeOutline();
    });

    const body = $('doc-body');
    body.addEventListener('click', (event) => {
      const anchor = event.target.closest('button.anchor');
      if (anchor) {
        history.replaceState(null, '', `#${encodeURIComponent(anchor.dataset.anchor)}`);
        share(anchor.dataset.anchor);
        return;
      }
      const cardEl = event.target.closest('.diagram-card.drawn');
      if (cardEl && !event.target.closest('a')) {
        openZoom(S.diagrams[Number(cardEl.dataset.diagram)]);
        return;
      }
      const local = event.target.closest('a[href^="#"]');
      if (local && !local.classList.contains('code-link') && !event.metaKey && !event.ctrlKey) {
        const id = decodeURIComponent(local.getAttribute('href').slice(1));
        if (document.getElementById(id)) {
          event.preventDefault();
          goTo(id, true);
        }
        return;
      }
      onCodeLink(event);
    });
    document.addEventListener('keydown', (event) => {
      const link = event.target.closest && event.target.closest('a.code-link:not([href])');
      if (link && event.key === 'Enter') {
        event.preventDefault();
        openInEditor(link);
      }
    });
    // Zooming from a card's button or a phone's tap goes through the click above; the button is focusable for
    // the keyboard, and Enter on it clicks.

    document.addEventListener('keydown', (event) => {
      if (event.defaultPrevented || event.metaKey || event.ctrlKey || event.altKey) return;
      const typing = event.target.closest && event.target.closest('input, textarea, select, [contenteditable="true"]');
      if (event.key === 'Escape') {
        if (!$('theme-menu').hidden) $('theme-menu').hidden = true;
        else if (document.body.classList.contains('outline-open')) closeOutline();
        else if (document.body.classList.contains('chat-open') && !isWide()) setChat(false);
        return;
      }
      if (typing || document.querySelector('dialog[open]')) return;
      if (event.key === '/') {
        event.preventDefault();
        if (isPhone()) document.body.classList.add('find-open');
        $('find').focus();
      } else if (event.key === 'c') {
        setChat(!document.body.classList.contains('chat-open'));
      } else if (event.key === 'j' && S.entries.length) {
        step(1);
      } else if (event.key === 'k' && S.entries.length) {
        step(-1);
      }
    });

    window.addEventListener('scroll', onScroll, { passive: true });
    let wasWide = isWide();
    let wasPhone = isPhone();
    window.addEventListener('resize', () => {
      if (isWide() !== wasWide) {
        wasWide = isWide();
        setChat(wasWide && store.get(CHAT_KEY) !== 'closed', false);
      }
      if (isPhone() !== wasPhone) {
        wasPhone = isPhone();
        closeOutline();
        if (S.wiki) watchHeadings();
      }
    });
    window.addEventListener('hashchange', () => {
      const id = decodeURIComponent(location.hash.slice(1));
      if (S.byId.has(id)) setActive(S.byId.get(id));
    });
  }

  function showError(message) {
    $('title').textContent = 'No wiki here';
    $('doc-body').innerHTML = `<div class="notice"><p>Couldn't show this wiki: ${esc(message)}.</p><p><code>crystal wiki build</code> in the project writes one; <code>crystal wiki serve</code> shows it.</p></div>`;
  }

  async function start() {
    applyTheme(themePref());
    wirePage();
    wireZoom();
    wireFind();
    let wiki;
    try {
      wiki = await load();
    } catch (err) {
      wireChat();
      setChat(false, false);
      showError(err.message || String(err));
      document.body.classList.remove('loading');
      return;
    }
    wireChat();
    S.wiki = wiki;
    // The mark leads back to the server's list of wikis; an export has nowhere to go.
    if (S.mode === 'serve') {
      $('logo').href = new URL('../../', location.href).pathname;
      $('logo').setAttribute('aria-label', 'crystal wiki: every wiki');
    }
    setChat(isWide() && store.get(CHAT_KEY) !== 'closed', false);
    renderWiki(wiki);
    document.body.classList.remove('loading');
    performance.mark('cw-content');
    const id = decodeURIComponent(location.hash.slice(1));
    if (id && document.getElementById(id)) {
      const el = document.getElementById(id);
      el.scrollIntoView({ block: 'start' });
      if (S.byId.has(id)) setActive(S.byId.get(id));
      // The fonts can move it once they're in; it's put back unless the reader has moved since.
      const y = scrollY;
      document.fonts.ready.then(() => {
        if (Math.abs(scrollY - y) < 2) el.scrollIntoView({ block: 'start' });
      });
    }
    if (S.mode === 'serve') pollStatus();
  }

  window.crystalWiki = { state: S, diagrams: D };
  start();
})();
