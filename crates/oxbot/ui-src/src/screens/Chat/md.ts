// Small, safe Markdown for bot replies, exactly as the hand-written page:
// escape everything, then add code blocks, inline code, bold and links.
// `mdHtml` builds the same HTML string the old page set as innerHTML;
// `mdTree` reads it back into plain nodes (only the tags mdHtml makes), which
// MdView draws as React elements, so nothing is ever set as HTML.

const esc = (s: string) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");

export function mdHtml(text: string): string {
  const parts = text.split(/```/);
  return parts
    .map((part, i) => {
      if (i % 2 === 1) return `<pre><code>${esc(part.replace(/^[^\n]*\n/, ""))}</code></pre>`;
      return esc(part)
        .split(/\n{2,}/)
        .map((para) =>
          para.trim()
            ? `<p>${para
                .replace(/`([^`\n]+)`/g, "<code>$1</code>")
                .replace(/\*\*([^*\n]+)\*\*/g, "<strong>$1</strong>")
                .replace(/\[([^\]\n]+)\]\((https?:\/\/[^\s)]+)\)/g, '<a href="$2" target="_blank" rel="noopener noreferrer">$1</a>')
                .replace(/(^|[\s(])(https?:\/\/[^\s<)]*[^\s<).,;:!?])/g, '$1<a href="$2" target="_blank" rel="noopener noreferrer">$2</a>')
                .replace(/\n/g, "<br>")}</p>`
            : "",
        )
        .join("");
    })
    .join("");
}

export type MdTag = "p" | "br" | "pre" | "code" | "strong" | "a";
export type MdNode = string | { tag: MdTag; href?: string; children: MdNode[] };

const unesc = (s: string) => s.replace(/&lt;/g, "<").replace(/&gt;/g, ">").replace(/&quot;/g, '"').replace(/&amp;/g, "&");
const TAG = /<(\/?)(p|br|pre|code|strong|a)(?: href="([^"]*)" target="_blank" rel="noopener noreferrer")?>/y;

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
