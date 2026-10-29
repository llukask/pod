// ==============================================================================
// Show notes
// ==============================================================================
//
// Episode notes come from arbitrary publishers. The server sanitizes them at
// ingest (spec 2.2), and the client treats them as untrusted anyway: it parses
// them with DOMParser, which never runs scripts or loads resources, and copies
// only the allowlisted elements into fresh nodes of this document. No string of
// publisher HTML is ever assigned to `innerHTML`.

/** The server's allowlist (spec 2.2). Anything else is unwrapped to its children. */
const KEPT = new Set([
  "p", "br", "a", "strong", "b", "em", "i", "u", "ul", "ol", "li", "blockquote", "code", "pre",
  "h1", "h2", "h3", "h4", "h5", "h6",
]);

/** Elements dropped with everything inside them: code, styling and embedded content. */
const DROPPED = new Set([
  "script", "style", "noscript", "template", "iframe", "object", "embed", "img", "picture", "video",
  "audio", "svg", "math", "form", "input", "button", "select", "textarea", "head", "title", "link", "meta",
]);

const SAFE_HREF = /^(https?:\/\/|mailto:)/i;

export function showNotes(source: string): DocumentFragment {
  const parsed = new DOMParser().parseFromString(source, "text/html").body;
  const out = document.createDocumentFragment();
  if (parsed.children.length === 0) {
    appendPlainText(out, parsed.textContent ?? "");
  } else {
    for (const child of Array.from(parsed.childNodes)) copy(child, out);
  }
  return out;
}

function copy(node: Node, into: Node) {
  if (node.nodeType === Node.TEXT_NODE) {
    into.appendChild(document.createTextNode(node.textContent ?? ""));
    return;
  }
  if (node.nodeType !== Node.ELEMENT_NODE) return;
  const element = node as Element;
  const tag = element.localName;
  if (DROPPED.has(tag)) return;
  if (!KEPT.has(tag)) {
    for (const child of Array.from(element.childNodes)) copy(child, into);
    return;
  }
  const clean = document.createElement(tag);
  if (tag === "a") {
    const href = element.getAttribute("href")?.trim();
    if (href && SAFE_HREF.test(href)) {
      clean.setAttribute("href", href);
      clean.setAttribute("target", "_blank");
      clean.setAttribute("rel", "noopener noreferrer nofollow");
    }
  }
  for (const child of Array.from(element.childNodes)) copy(child, clean);
  into.appendChild(clean);
}

/** Feeds that send plain text: blank lines separate paragraphs, single newlines break lines. */
function appendPlainText(into: Node, text: string) {
  if (text.trim() === "") return;
  for (const paragraph of text.trim().split(/\n\s*\n/)) {
    const p = document.createElement("p");
    paragraph.split("\n").forEach((line, index) => {
      if (index > 0) p.appendChild(document.createElement("br"));
      p.appendChild(document.createTextNode(line));
    });
    into.appendChild(p);
  }
}
