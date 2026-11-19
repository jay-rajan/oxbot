// Keeps the page the size of what's visible. A phone's keyboard covers the
// bottom of the screen; some browsers then scroll the whole page up to show
// the message box, pushing the header under the status bar. Here the page
// shrinks to the visible height (--app-h, read in theme/base.css) and is
// put back at the top, so only the chat inside it scrolls.
export function fitToVisible(win: Window = window): void {
  const vv = win.visualViewport;
  if (!vv) return;
  const root = win.document.documentElement;
  const fit = () => {
    root.style.setProperty("--app-h", `${Math.round(vv.height)}px`);
    if (win.scrollY || win.scrollX) win.scrollTo(0, 0);
  };
  vv.addEventListener("resize", fit);
  vv.addEventListener("scroll", fit);
  fit();
}
