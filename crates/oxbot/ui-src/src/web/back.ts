// The phone's Back (Android's back swipe) goes back in the page's history,
// and with nowhere to go back to it leaves the app. So each open window
// (a dialog, the bot drawer) adds a history entry, and going back closes
// the top one instead. In a browser, its Back button does the same.
import { useEffect, useRef } from "react";

type Layer = { close: () => void };

const open: Layer[] = [];
/** Back steps the page took itself (a window closed by its own button). */
let ours = 0;

function onPop() {
  if (ours > 0) {
    ours -= 1;
    return;
  }
  open.pop()?.close();
}

let listening = false;

/** Adds a window that Back closes (calling `close`). Returns what to call
 *  when the page closes it itself, which takes its history entry back out. */
export function addLayer(close: () => void): () => void {
  if (!listening) {
    window.addEventListener("popstate", onPop);
    listening = true;
  }
  const layer = { close };
  open.push(layer);
  history.pushState(history.state, "");
  return () => {
    const i = open.indexOf(layer);
    // Back closed it already: its entry is gone.
    if (i < 0) return;
    open.splice(i, 1);
    ours += 1;
    history.back();
  };
}

/** While `isOpen`, Back calls `close` instead of leaving the page. */
export function useBackCloses(isOpen: boolean, close: () => void) {
  const latest = useRef(close);
  latest.current = close;
  useEffect(() => {
    if (!isOpen) return;
    return addLayer(() => latest.current());
  }, [isOpen]);
}

/** For tests: forgets every window. */
export function resetLayers() {
  open.length = 0;
  ours = 0;
}
