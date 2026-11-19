// Keeps the page the size of what's visible. A phone's keyboard covers the
// bottom of the screen; some browsers then scroll the whole page up to show
// the message box, pushing the header under the status bar. Here the page
// shrinks to the visible height (--app-h, read in theme/base.css) and is
// put back at the top, so only the chat inside it scrolls. While the
// keyboard is up (the visible height well under the tallest seen), the root
// has the class kb-open, so the phone's tab bar can step aside.
export function fitToVisible(win: Window = window): void {
  const vv = win.visualViewport;
  if (!vv) return;
  const root = win.document.documentElement;
  let tallest = 0;
  let wide = 0;
  const fit = () => {
    // A turned phone is a new screen: start measuring again.
    if (Math.round(vv.width) !== wide) {
      wide = Math.round(vv.width);
      tallest = 0;
    }
    tallest = Math.max(tallest, vv.height);
    root.style.setProperty("--app-h", `${Math.round(vv.height)}px`);
    root.classList.toggle("kb-open", vv.height < tallest * 0.8);
    if (win.scrollY || win.scrollX) win.scrollTo(0, 0);
  };
  vv.addEventListener("resize", fit);
  vv.addEventListener("scroll", fit);
  fit();
}
