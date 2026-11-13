// Compatibility names keep Oxbot's existing approval and replay rules intact.
const text = { type: "string" },
  integer = { type: "integer" };
const spec = (name, description, properties = {}, required = []) => ({
  name,
  description,
  inputSchema: {
    type: "object",
    properties,
    required,
    additionalProperties: false,
  },
});
export const tools = [
  spec("navigate_page", "Navigate the selected page once.", {
    url: text,
    type: { type: "string", enum: ["url", "back", "forward", "reload"] },
  }),
  spec(
    "new_page",
    "Open a web page in the current browser window.",
    { url: text },
    ["url"],
  ),
  spec("list_pages", "List open pages."),
  spec("select_page", "Select a page.", { pageId: integer }, ["pageId"]),
  spec("close_page", "Close a page.", { pageId: integer }, ["pageId"]),
  spec(
    "take_snapshot",
    "Read a fresh accessibility snapshot. Use its element uids for actions.",
  ),
  spec("take_screenshot", "Capture the selected page.", {
    filePath: text,
    fullPage: { type: "boolean" },
  }),
  spec(
    "wait_for",
    "Wait for text already loading; this does not reload.",
    { text: { type: "array", items: text }, timeout: integer },
    ["text"],
  ),
  spec(
    "click",
    "Click a fresh element uid.",
    { uid: text, dblClick: { type: "boolean" } },
    ["uid"],
  ),
  spec("hover", "Hover over an element.", { uid: text }, ["uid"]),
  spec("fill", "Fill an input once.", { uid: text, value: text }, [
    "uid",
    "value",
  ]),
  spec(
    "fill_form",
    "Fill fields once.",
    {
      elements: {
        type: "array",
        items: {
          type: "object",
          properties: { uid: text, value: text },
          required: ["uid", "value"],
          additionalProperties: false,
        },
      },
    },
    ["elements"],
  ),
  spec("press_key", "Press one key combination.", { key: text }, ["key"]),
  spec(
    "drag",
    "Drag between two fresh element uids.",
    { from_uid: text, to_uid: text },
    ["from_uid", "to_uid"],
  ),
  spec(
    "upload_file",
    "Upload approved files inside the bot workspace.",
    { uid: text, filePath: text, filePaths: { type: "array", items: text } },
    ["uid"],
  ),
  spec(
    "handle_dialog",
    "Answer an open dialog.",
    {
      action: { type: "string", enum: ["accept", "dismiss"] },
      promptText: text,
    },
    ["action"],
  ),
];
export const names = new Set(tools.map((tool) => tool.name));
