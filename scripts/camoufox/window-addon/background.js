// No website permissions, content scripts, web-accessible resources, external
// messaging, URLs, or page data. Commands come only from this launch's private
// loopback capability. This runs independently of a minimized page's renderer.
(async () => {
  const { endpoint } = await (
    await fetch(browser.runtime.getURL("bridge.json"))
  ).json();
  let minimized = true;
  const changing = new Set();
  async function enforce(id) {
    if (!minimized || changing.has(id) || id === browser.windows.WINDOW_ID_NONE)
      return;
    changing.add(id);
    try {
      const window = await browser.windows.get(id);
      if (minimized && window.type === "normal" && window.state !== "minimized")
        await browser.windows.update(id, { state: "minimized" });
    } catch {
    } finally {
      changing.delete(id);
    }
  }
  browser.windows.onCreated.addListener((window) => {
    void enforce(window.id);
  });
  // Firefox may restore a window when it selects a tab during navigation.
  // Restore-for-user disables enforcement before bringing that window forward.
  browser.windows.onFocusChanged.addListener((id) => {
    void enforce(id);
  });
  browser.windows.onBoundsChanged?.addListener((window) => {
    void enforce(window.id);
  });
  for (;;) {
    try {
      const response = await fetch(`${endpoint}/next`, { cache: "no-store" });
      if (response.status === 410) return;
      if (response.status === 204) continue;
      if (!response.ok) throw new Error("Window bridge unavailable");
      const { id, state } = await response.json();
      let result;
      try {
        if (!["minimized", "normal", "maximized", "status"].includes(state))
          throw new Error("Invalid window state");
        const windows = await browser.windows.getAll({
          windowTypes: ["normal"],
        });
        if (state !== "status") {
          minimized = state === "minimized";
          for (const window of windows) {
            await browser.windows.update(
              window.id,
              state === "minimized" ? { state } : { state, focused: true },
            );
          }
        }
        result = {
          id,
          states: (
            await browser.windows.getAll({ windowTypes: ["normal"] })
          ).map((w) => w.state),
        };
      } catch {
        result = { id, error: "Could not change browser window state" };
      }
      await fetch(`${endpoint}/result`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(result),
      });
    } catch {
      await new Promise((resolve) => setTimeout(resolve, 1000));
    }
  }
})();
