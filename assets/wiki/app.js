// A stand-in for the wiki's page until its own lands: it shows the wiki's
// text and diagrams plainly, and asks questions when served.
(function () {
  const page = document.getElementById("page");
  const inline = document.getElementById("wiki-data");
  const escape = (text) => text.replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);
  const markdown = (text) =>
    escape(text || "")
      .replace(/`([^`]+)`/g, "<code>$1</code>")
      .replace(/\[([^\]]+)\]\(([^)]+)\)/g, (_, label, href) => `<a href="${href}">${label}</a>`)
      .split(/\n{2,}/)
      .map((block) => `<p>${block}</p>`)
      .join("");
  const diagram = (d) => (d ? `<pre class="diagram mermaid">${escape(d.mermaid)}</pre>` : "");
  function render(wiki) {
    let html = `<h1>${escape(wiki.repo.name)}</h1>${diagram(wiki.overview.diagram)}${markdown(wiki.overview.summary_md)}`;
    for (const section of wiki.sections) {
      html += `<h2 id="${escape(section.id)}">${escape(section.title)}</h2>${diagram(section.diagram)}${markdown(section.summary_md)}`;
      for (const sub of section.subsections || []) {
        html += `<h3 id="${escape(sub.id)}">${escape(sub.title)}</h3>${diagram(sub.diagram)}${markdown(sub.body_md)}`;
      }
    }
    page.innerHTML = html;
    document.title = wiki.repo.name;
    if (window.mermaid) window.mermaid.run({ querySelector: ".mermaid" });
  }
  if (inline) {
    render(JSON.parse(inline.textContent));
    return;
  }
  fetch("wiki.json")
    .then((response) => (response.ok ? response.json() : Promise.reject(response.statusText)))
    .then(render)
    .catch((why) => (page.innerHTML = `<p class="muted">No wiki here: ${escape(String(why))}</p>`));
  const chat = document.getElementById("chat");
  const answers = document.getElementById("answers");
  let conversation = null;
  chat.hidden = false;
  document.getElementById("ask").addEventListener("submit", async (event) => {
    event.preventDefault();
    const input = event.target.question;
    const answer = document.createElement("p");
    answers.append(answer);
    const response = await fetch("api/ask", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ question: input.value, conversation, section: null }),
    });
    input.value = "";
    const reader = response.body.pipeThrough(new TextDecoderStream()).getReader();
    let buffer = "";
    for (;;) {
      const { value, done } = await reader.read();
      if (done) break;
      buffer += value;
      let end;
      while ((end = buffer.indexOf("\n\n")) >= 0) {
        const message = buffer.slice(0, end);
        buffer = buffer.slice(end + 2);
        const kind = (message.match(/^event: (.*)$/m) || [])[1];
        const data = JSON.parse((message.match(/^data: (.*)$/m) || [, "{}"])[1]);
        if (kind === "delta") answer.textContent += data.text;
        if (kind === "done") conversation = data.conversation;
        if (kind === "error") answer.textContent += ` (${data.message})`;
      }
    }
  });
})();
