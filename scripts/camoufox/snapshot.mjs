// Turns Playwright's AI aria snapshot into the page text oxbot's loop reads
// (the chrome-devtools-mcp 1.10.1 layout, see crates/oxbot-loop/src/page):
// one `uid=` line per node, two spaces per depth under the RootWebArea,
// `role "name"` then attributes as `key="value"` or a bare `key`. Link
// addresses become `url="…"` (absolute), a text box's text `value="…"`,
// and loose text `StaticText "…"`. Pure: no browser needed, unit tested.

const MAX_TEXT = 2000;
const MAX_LINES = 20000;
const MAX_CHARS = 400000;
/// Roles whose inline text is the field's value, not a child text.
const VALUE_ROLES = new Set(["textbox", "searchbox", "combobox", "spinbutton", "slider"]);
/// Playwright's state flags, as the loop names them. `ref` and `cursor`
/// are Playwright's own and are dropped.
const FLAGS = { active: "focused" };
const DROPPED = new Set(["ref", "cursor"]);

const clean = (text) =>
  String(text ?? "")
    .replace(/[\r\n\u2028\u2029]/g, " ")
    .slice(0, MAX_TEXT);

/// A YAML scalar as Playwright writes one: plain, 'single' or "double".
function scalar(text) {
  const t = text.trim();
  if (t.startsWith('"')) {
    try {
      return JSON.parse(t);
    } catch {
      return t.slice(1, -1);
    }
  }
  if (t.startsWith("'") && t.endsWith("'") && t.length >= 2)
    return t.slice(1, -1).replace(/''/g, "'");
  return t;
}

/// Splits `key: value` at the first `:` outside quotes and brackets that
/// ends the key (followed by a space or the end).
function splitKey(body) {
  let quote = null,
    depth = 0;
  for (let i = 0; i < body.length; i++) {
    const c = body[i];
    if (quote) {
      if (c === "\\" && quote === '"') i++;
      else if (c === quote) quote = null;
    } else if (c === '"' || (c === "'" && i === 0)) quote = c;
    else if (c === "[") depth++;
    else if (c === "]") depth--;
    else if (c === ":" && depth === 0 && (i + 1 === body.length || body[i + 1] === " "))
      return [body.slice(0, i), body.slice(i + 1).trim()];
  }
  return [body, undefined];
}

/// `role "name" [flag] [key=value]…` → its parts, or null for loose text.
function parseKey(key) {
  const k = key.startsWith("'") ? scalar(key) : key;
  const m = k.match(/^([A-Za-z][\w-]*)(?:\s+("(?:[^"\\]|\\.)*"))?((?:\s+\[[^\]]*\])*)\s*$/);
  if (!m) return null;
  let name = "";
  if (m[2]) {
    try {
      name = JSON.parse(m[2]);
    } catch {
      name = m[2].slice(1, -1);
    }
  }
  const attrs = [];
  let ref;
  for (const [, flag, value] of m[3].matchAll(/\[([^\]=]+)(?:=([^\]]*))?\]/g)) {
    if (flag === "ref") ref = value;
    if (DROPPED.has(flag)) continue;
    attrs.push([FLAGS[flag] ?? flag, value]);
  }
  return { role: m[1], name, attrs, ref };
}

const absolute = (href, base) => {
  try {
    return new URL(href, base).href;
  } catch {
    return href;
  }
};

/// The page's snapshot lines. `uid(n)` names the n-th node; returns the
/// lines and, for each node with a Playwright ref, `{ uid, ref, role, name }`.
export function toDevtools(aria, { title = "", url = "", uid }) {
  const nodes = [];
  // The node last seen at each depth, so properties find their owner.
  const stack = [];
  const add = (depth, role, name, attrs = [], ref) => {
    const node = { depth, role, name: clean(name), raw: name, attrs, ref };
    nodes.push(node);
    stack[depth] = node;
    stack.length = depth + 1;
    return node;
  };
  for (const raw of aria.split("\n").slice(0, MAX_LINES)) {
    const m = raw.match(/^(\s*)- (.*)$/);
    if (!m) continue;
    const depth = Math.floor(m[1].length / 2);
    const [key, value] = splitKey(m[2]);
    // `- /url: …`, `- /placeholder: …`: a property of the node above.
    if (key.startsWith("/")) {
      const owner = stack[depth - 1];
      if (owner && value !== undefined) {
        const prop = key.slice(1);
        const text = scalar(value);
        owner.attrs.push([prop, prop === "url" ? absolute(text, url) : clean(text)]);
      }
      continue;
    }
    if (key === "text") {
      add(depth, "StaticText", value === undefined ? "" : scalar(value));
      continue;
    }
    const parsed = parseKey(key);
    if (!parsed) {
      add(depth, "StaticText", scalar(value === undefined ? key : `${key}: ${value}`));
      continue;
    }
    const node = add(depth, parsed.role, parsed.name, parsed.attrs, parsed.ref);
    if (value) {
      const text = scalar(value);
      if (VALUE_ROLES.has(parsed.role)) node.attrs.push(["value", clean(text)]);
      else add(depth + 1, "StaticText", text);
    }
  }
  const lines = [
    `uid=${uid("root")} RootWebArea ${JSON.stringify(clean(title))} url=${JSON.stringify(clean(url))}`,
  ];
  const refs = [];
  let size = lines[0].length,
    n = 0;
  for (const node of nodes) {
    const id = uid(n++);
    let line = `${"  ".repeat(node.depth + 1)}uid=${id} ${node.role}`;
    if (node.name || node.role === "StaticText") line += ` ${JSON.stringify(node.name)}`;
    for (const [k, v] of node.attrs)
      line += v === undefined ? ` ${k}` : ` ${k}=${JSON.stringify(clean(v))}`;
    size += line.length + 1;
    if (size > MAX_CHARS) break;
    lines.push(line);
    if (node.ref) refs.push({ uid: id, ref: node.ref, role: node.role, name: node.raw });
  }
  return { lines, refs };
}
