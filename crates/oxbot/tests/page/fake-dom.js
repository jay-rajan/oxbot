// The page script run in Node against a do-nothing DOM, shared by the page tests.
function fake() {
  const f = function () { return fake(); };
  return new Proxy(f, {
    get(t, k) {
      if (k === Symbol.toPrimitive) return () => "";
      if (k === "length" ) return 0;
      if (k === Symbol.iterator) return function* () {};
      if (k in t && typeof k === "string" && ["scrollTop","scrollHeight"].includes(k)) return t[k];
      if (k === "then") return undefined;
      if (["scrollTop", "scrollHeight", "clientHeight"].includes(k)) return 0;
      if (k === "dataset" || k === "style" || k === "classList") return fake();
      return t[k] !== undefined && typeof k === "string" && !["name","call","apply","bind"].includes(k) ? t[k] : fake();
    },
    set(t, k, v) { t[k] = v; return true; },
    apply() { return fake(); },
  });
}
// Runs the page script `src` and returns what `expose` (a JS object
// expression over the page's names) gives back.
function load(src, expose) {
  const ctx = {
    document: fake(), window: { addEventListener() {} }, location: { hash: "", search: "", pathname: "/", host: "x" },
    history: { replaceState() {} }, sessionStorage: { getItem() { return ""; }, setItem() {} },
    localStorage: { getItem() { return null; }, setItem() {} }, navigator: {}, requestAnimationFrame() {},
    setTimeout() {}, clearTimeout() {}, setInterval() {}, clearInterval() {}, WebSocket: function () { return fake(); },
    console, TextEncoder, URLSearchParams, Math, JSON, Date, Map, Set, Array, Object, String, Number, Promise,
    encodeURIComponent, matchMedia: () => fake(), Intl, alert() {}, confirm() { return true; },
  };
  const vm = require("vm");
  vm.createContext(ctx);
  vm.runInContext(src + "\n;globalThis.__t = " + expose + ";", ctx);
  return ctx.__t;
}
module.exports = { fake, load };
