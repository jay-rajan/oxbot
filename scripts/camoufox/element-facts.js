function elementFacts() {
  const get = (cls, key, obj) =>
    Object.getOwnPropertyDescriptor(cls.prototype, key).get.call(obj);
  const attr = (el, key) =>
    (Element.prototype.getAttribute.call(el, key) || "").toLowerCase();
  const e = this;
  if (!(e instanceof Element)) return null;
  const tag = get(Element, "tagName", e).toLowerCase();
  let type = attr(e, "type"),
    form = null;
  if (e instanceof HTMLButtonElement) {
    type = get(HTMLButtonElement, "type", e);
    form = get(HTMLButtonElement, "form", e);
  } else if (e instanceof HTMLInputElement) {
    type = get(HTMLInputElement, "type", e);
    form = get(HTMLInputElement, "form", e);
  } else form = Element.prototype.closest.call(e, "form");
  const submits =
    !!form &&
    ((tag === "button" && type === "submit") ||
      (tag === "input" && (type === "submit" || type === "image")));
  let method = null;
  if (form) {
    const m = (submits && attr(e, "formmethod")) || attr(form, "method");
    method = m === "post" || m === "dialog" ? m : "get";
  }
  const inSearch =
    !!Element.prototype.closest.call(e, "[role=search],search") ||
    (!!form &&
      (attr(form, "role") === "search" ||
        !!Element.prototype.querySelector.call(form, "input[type=search]")));
  let filled = 0;
  if (form) {
    const fields = get(HTMLFormElement, "elements", form);
    for (let i = 0; i < fields.length; i++) {
      const f = fields[i];
      if (f instanceof HTMLInputElement) {
        const t = get(HTMLInputElement, "type", f);
        if (
          ["hidden", "submit", "button", "reset", "image", "search"].includes(t)
        )
          continue;
        if (t === "checkbox" || t === "radio") {
          if (get(HTMLInputElement, "checked", f)) filled++;
        } else if (t === "file") {
          const files = get(HTMLInputElement, "files", f);
          if (files && files.length) filled++;
        } else if (get(HTMLInputElement, "value", f).trim()) filled++;
      } else if (f instanceof HTMLTextAreaElement) {
        if (get(HTMLTextAreaElement, "value", f).trim()) filled++;
      } else if (f instanceof HTMLSelectElement) {
        if (get(HTMLSelectElement, "selectedIndex", f) > 0) filled++;
      }
    }
  }
  return { tag, type, submits, method, inSearch, filled };
}
