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
      { tag: "p", children: ["x"] },
      { tag: "pre", children: [{ tag: "code", children: ["let a = '<b>';\n"] }] },
    ]);
  });

  it("no javascript links", () => {
    expect(mdHtml("[x](javascript:alert(1))")).not.toContain("href");
  });

  it("headings, lists and tables", () => {
    expect(md("## Monday\n- 1 lb chicken\n- 2 *peppers*\n\n1. Cook rice\n2. Stir-fry")).toEqual([
      { tag: "h3", children: ["Monday"] },
      { tag: "ul", children: [{ tag: "li", children: ["1 lb chicken"] }, { tag: "li", children: ["2 ", { tag: "em", children: ["peppers"] }] }] },
      { tag: "ol", children: [{ tag: "li", children: ["Cook rice"] }, { tag: "li", children: ["Stir-fry"] }] },
    ]);
    expect(md("| Day | Meal |\n|---|---|\n| Mon | **Tacos** |\n| Tue |")).toEqual([
      {
        tag: "table",
        children: [
          { tag: "thead", children: [{ tag: "tr", children: [{ tag: "th", children: ["Day"] }, { tag: "th", children: ["Meal"] }] }] },
          {
            tag: "tbody",
            children: [
              { tag: "tr", children: [{ tag: "td", children: ["Mon"] }, { tag: "td", children: [{ tag: "strong", children: ["Tacos"] }] }] },
              { tag: "tr", children: [{ tag: "td", children: ["Tue"] }, { tag: "td", children: [] }] },
            ],
          },
        ],
      },
    ]);
  });

  it("pipes without a rule line stay text, and 2 * 3 * 4 isn't italics", () => {
    expect(md("a | b |")).toEqual([{ tag: "p", children: ["a | b |"] }]);
    expect(md("2 * 3 * 4")).toEqual([{ tag: "p", children: ["2 * 3 * 4"] }]);
  });
});
