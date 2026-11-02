//! What oxbot reads about one element on a page before a click: is it a
//! form's submit button, and what kind of form. Read in an isolated world
//! (see the guard), with the DOM's own getters, so the page's scripts and
//! form fields named like properties (`<input name="method">`) can't change
//! the answer.

use serde_json::Value;

/// What oxbot read about an element itself.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ElementFacts {
    /// Its tag, lower case (`button`, `input`, `a`…).
    pub tag: String,
    /// Its type, lower case (`submit`, `button`, `search`), or empty.
    pub input_type: String,
    /// Whether clicking it submits a form: a submit button, or a `<button>`
    /// with no type inside a form.
    pub submits: bool,
    /// The method a submit would use (`get`, `post` or `dialog`): the
    /// button's `formmethod`, else the form's, else `get`. `None` outside a
    /// form.
    pub form_method: Option<String>,
    /// Whether it sits in a search area (`role=search`, `<search>`) or its
    /// form has a search box.
    pub in_search: bool,
    /// How many of the form's fields have something in them (text, a
    /// choice, a ticked box), search boxes and hidden fields left out.
    pub filled_fields: u32,
}

impl ElementFacts {
    /// Reads what [`ELEMENT_FACTS_FN`] returned; `None` if it isn't an
    /// object.
    #[must_use]
    pub fn from_value(v: &Value) -> Option<Self> {
        let map = v.as_object()?;
        let text = |key: &str| {
            map.get(key)
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_lowercase()
        };
        let form_method = map
            .get("method")
            .and_then(Value::as_str)
            .map(str::to_lowercase);
        Some(Self {
            tag: text("tag"),
            input_type: text("type"),
            submits: map.get("submits").and_then(Value::as_bool) == Some(true),
            form_method,
            in_search: map.get("inSearch").and_then(Value::as_bool) == Some(true),
            filled_fields: map
                .get("filled")
                .and_then(Value::as_u64)
                .and_then(|n| u32::try_from(n).ok())
                .unwrap_or(0),
        })
    }
}

/// A function (called with the element as `this`) that returns the facts
/// [`ElementFacts::from_value`] reads. It only reads. Every DOM property is
/// read through the prototype's own getter, because a form's fields can
/// shadow its properties by name.
pub const ELEMENT_FACTS_FN: &str = r"function () {
  const get = (cls, key, obj) => Object.getOwnPropertyDescriptor(cls.prototype, key).get.call(obj);
  const attr = (el, key) => (Element.prototype.getAttribute.call(el, key) || '').toLowerCase();
  const e = this;
  if (!(e instanceof Element)) return null;
  const tag = get(Element, 'tagName', e).toLowerCase();
  let type = attr(e, 'type'), form = null;
  if (e instanceof HTMLButtonElement) { type = get(HTMLButtonElement, 'type', e); form = get(HTMLButtonElement, 'form', e); }
  else if (e instanceof HTMLInputElement) { type = get(HTMLInputElement, 'type', e); form = get(HTMLInputElement, 'form', e); }
  else form = Element.prototype.closest.call(e, 'form');
  const submits = !!form && ((tag === 'button' && type === 'submit') || (tag === 'input' && (type === 'submit' || type === 'image')));
  let method = null;
  if (form) {
    const m = (submits && attr(e, 'formmethod')) || attr(form, 'method');
    method = m === 'post' || m === 'dialog' ? m : 'get';
  }
  const inSearch = !!Element.prototype.closest.call(e, '[role=search],search')
    || (!!form && (attr(form, 'role') === 'search' || !!Element.prototype.querySelector.call(form, 'input[type=search]')));
  let filled = 0;
  if (form) {
    const fields = get(HTMLFormElement, 'elements', form);
    for (let i = 0; i < fields.length; i++) {
      const f = fields[i];
      if (f instanceof HTMLInputElement) {
        const t = get(HTMLInputElement, 'type', f);
        if (['hidden', 'submit', 'button', 'reset', 'image', 'search'].includes(t)) continue;
        if (t === 'checkbox' || t === 'radio') { if (get(HTMLInputElement, 'checked', f)) filled++; }
        else if (t === 'file') { const files = get(HTMLInputElement, 'files', f); if (files && files.length) filled++; }
        else if (get(HTMLInputElement, 'value', f).trim()) filled++;
      } else if (f instanceof HTMLTextAreaElement) {
        if (get(HTMLTextAreaElement, 'value', f).trim()) filled++;
      } else if (f instanceof HTMLSelectElement) {
        if (get(HTMLSelectElement, 'selectedIndex', f) > 0) filled++;
      }
    }
  }
  return { tag, type, submits, method, inSearch, filled };
}";

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn facts_are_read_from_the_function_result() {
        let v = json!({"tag":"BUTTON","type":"submit","submits":true,"method":"POST","inSearch":false,"filled":3});
        let facts = ElementFacts::from_value(&v).unwrap();
        assert_eq!(facts.tag, "button");
        assert!(facts.submits);
        assert_eq!(facts.form_method.as_deref(), Some("post"));
        assert_eq!(facts.filled_fields, 3);
        let outside = ElementFacts::from_value(&json!({"tag":"a","method":null})).unwrap();
        assert!(!outside.submits && outside.form_method.is_none());
        assert!(ElementFacts::from_value(&Value::Null).is_none());
    }

    #[test]
    fn the_function_only_reads() {
        // No assignments to the page: no `=` that isn't a comparison or a
        // local `const`/`let`, and no calls that change things.
        for banned in [
            ".click(",
            ".submit(",
            "requestSubmit",
            "dispatchEvent",
            "setAttribute",
            "removeAttribute",
            "innerHTML",
            ".value =",
            ".checked =",
            "fetch(",
            "XMLHttpRequest",
            "postMessage",
            "localStorage",
            "document.cookie",
        ] {
            assert!(!ELEMENT_FACTS_FN.contains(banned), "{banned}");
        }
    }
}
