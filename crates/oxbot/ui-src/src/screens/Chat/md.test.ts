import { describe, expect, it } from "vitest";
import { md, mdHtml } from "./md";

describe("md", () => {
  it("escapes HTML and keeps it as text", () => {
    expect(md("<img src=x onerror=alert(1)>")).toEqual([{ tag: "p", children: ["<img src=x onerror=alert(1)>"] }]);
  });

  it("makes paragraphs, line breaks, code, bold and links", () => {
    expect(md("a **b** `c`\nd\n\ne")).toEqual([
      { tag: "p", children: ["a ", { tag: "strong", children: ["b"] }, " ", { tag: "code", children: ["c"] }, { tag: "br", children: [] }, "d"] },
      { tag: "p", children: ["e"] },
    ]);
    expect(md("[site](https://x.com/a?b=1&c=2)")).toEqual([
      { tag: "p", children: [{ tag: "a", href: "https://x.com/a?b=1&c=2", children: ["site"] }] },
    ]);
    expect(md("see https://x.com/page.")).toEqual([
      { tag: "p", children: ["see ", { tag: "a", href: "https://x.com/page", children: ["https://x.com/page"] }, "."] },
    ]);
  });

  it("bold may wrap code, as before", () => {
    expect(md("**use `npm`**")).toEqual([
      { tag: "p", children: [{ tag: "strong", children: ["use ", { tag: "code", children: ["npm"] }] }] },
    ]);
  });

  it("code blocks drop their language line and keep text as is", () => {
    expect(md("x\n```js\nlet a = '<b>';\n```")).toEqual([
      { tag: "p", children: ["x", { tag: "br", children: [] }] },
      { tag: "pre", children: [{ tag: "code", children: ["let a = '<b>';\n"] }] },
    ]);
  });

  it("no javascript links", () => {
    expect(mdHtml("[x](javascript:alert(1))")).not.toContain("href");
  });
});
