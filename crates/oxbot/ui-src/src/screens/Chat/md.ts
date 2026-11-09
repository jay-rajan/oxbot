// Small, safe Markdown for bot replies: escape everything, then add code
// blocks, headings, bulleted and numbered lists, tables, inline code, bold,
// italics and links.
// `mdHtml` builds the same HTML string the old page set as innerHTML;
// `mdTree` reads it back into plain nodes (only the tags mdHtml makes), which
// MdView draws as React elements, so nothing is ever set as HTML.

const esc = (s: string) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");

/** Bold, italics, inline code and links inside one line (already escaped). */
function inline(t: string): string {
  return t
    .replace(/`([^`\n]+)`/g, "<code>$1</code>")
    .replace(/\*\*([^*\n]+)\*\*/g, "<strong>$1</strong>")
    .replace(/(^|[\s(])\*([^*\s][^*\n]*?)\*(?=[\s).,;:!?]|$)/g, "$1<em>$2</em>")
    .replace(/\[([^\]\n]+)\]\((https?:\/\/[^\s)]+)\)/g, '<a href="$2" target="_blank" rel="noopener noreferrer">$1</a>')
    .replace(/(^|[\s(])(https?:\/\/[^\s<)]*[^\s<).,;:!?])/g, '$1<a href="$2" target="_blank" rel="noopener noreferrer">$2</a>');
}

const HEADING = /^(#{1,6})\s+(.*)$/;
const BULLET = /^\s*[-*•]\s+(.*)$/;
const NUMBERED = /^\s*\d+[.)]\s+(.*)$/;
const ROW = /^\s*\|.*\|\s*$/;
const RULE = /^\s*\|?\s*:?-{2,}:?\s*(\|\s*:?-{2,}:?\s*)*\|?\s*$/;

const cells = (line: string) =>
  line
    .trim()
    .replace(/^\|/, "")
    .replace(/\|$/, "")
    .split("|")
    .map((c) => inline(c.trim()));

/** One stretch of text between code blocks, as blocks: headings, lists,
 *  tables and paragraphs. */
function blocks(text: string): string {
  const lines = esc(text).split("\n");
  const out: string[] = [];
  let para: string[] = [];
  const endPara = () => {
    if (para.some((l) => l.trim())) out.push(`<p>${para.map(inline).join("<br>")}</p>`);
    para = [];
  };
  let i = 0;
  while (i < lines.length) {
    const line = lines[i] ?? "";
    const h = HEADING.exec(line);
    if (!line.trim()) {
      endPara();
      i++;
    } else if (h) {
      endPara();
      out.push(`<${(h[1] ?? "").length <= 2 ? "h3" : "h4"}>${inline(h[2] ?? "")}</${(h[1] ?? "").length <= 2 ? "h3" : "h4"}>`);
      i++;
    } else if (ROW.test(line) && RULE.test(lines[i + 1] ?? "")) {
      endPara();
      const head = cells(line);
      i += 2;
      const body: string[][] = [];
      while (i < lines.length && ROW.test(lines[i] ?? "")) body.push(cells(lines[i++] ?? ""));
      out.push(
        `<table><thead><tr>${head.map((c) => `<th>${c}</th>`).join("")}</tr></thead><tbody>${body
          .map((r) => `<tr>${head.map((_, k) => `<td>${r[k] ?? ""}</td>`).join("")}</tr>`)
          .join("")}</tbody></table>`,
      );
    } else if (BULLET.test(line) || NUMBERED.test(line)) {
      endPara();
      const numbered = !BULLET.test(line);
      const re = numbered ? NUMBERED : BULLET;
      const items: string[] = [];
      while (i < lines.length && re.test(lines[i] ?? "")) items.push(inline(re.exec(lines[i++] ?? "")?.[1] ?? ""));
      const tag = numbered ? "ol" : "ul";
      out.push(`<${tag}>${items.map((x) => `<li>${x}</li>`).join("")}</${tag}>`);
    } else {
      para.push(line);
      i++;
    }
  }
  endPara();
  return out.join("");
}

export function mdHtml(text: string): string {
  return text
    .split(/```/)
    .map((part, i) => (i % 2 === 1 ? `<pre><code>${esc(part.replace(/^[^\n]*\n/, ""))}</code></pre>` : blocks(part)))
    .join("");
}

export type MdTag = "p" | "br" | "pre" | "code" | "strong" | "em" | "a" | "h3" | "h4" | "ul" | "ol" | "li" | "table" | "thead" | "tbody" | "tr" | "th" | "td";
export type MdNode = string | { tag: MdTag; href?: string; children: MdNode[] };

const unesc = (s: string) => s.replace(/&lt;/g, "<").replace(/&gt;/g, ">").replace(/&quot;/g, '"').replace(/&amp;/g, "&");
const TAG = /<(\/?)(p|br|pre|code|strong|em|a|h3|h4|ul|ol|li|table|thead|tbody|tr|th|td)(?: href="([^"]*)" target="_blank" rel="noopener noreferrer")?>/y;

/** Reads mdHtml's output back into nodes. Overlapping tags close the way a
 *  browser would (the inner ones end with the outer). */
export function mdTree(html: string): MdNode[] {
  const root: { tag: MdTag | "root"; href?: string; children: MdNode[] } = { tag: "root", children: [] };
  const stack = [root];
  let text = "";
  const top = () => stack[stack.length - 1] as (typeof stack)[number];
  const flush = () => {
    if (text) top().children.push(unesc(text));
    text = "";
  };
  let i = 0;
  while (i < html.length) {
    if (html[i] === "<") {
      TAG.lastIndex = i;
      const m = TAG.exec(html);
      if (m) {
        flush();
        const [all, close, tag, href] = m as unknown as [string, string, MdTag, string | undefined];
        i += all.length;
        if (tag === "br") top().children.push({ tag: "br", children: [] });
        else if (!close) {
          const node = { tag, children: [] as MdNode[], ...(href !== undefined ? { href: unesc(href) } : {}) };
          top().children.push(node);
          stack.push(node);
        } else if (stack.some((n, k) => k > 0 && n.tag === tag)) {
          while (stack.length > 1 && top().tag !== tag) stack.pop();
          stack.pop();
        }
        continue;
      }
    }
    text += html[i];
    i++;
  }
  flush();
  return root.children;
}

/** Markdown text as nodes. */
export function md(text: string): MdNode[] {
  return mdTree(mdHtml(text));
}
