// The panel fits its window (.app is 100vh) and every view scrolls inside .main-scroll, so the window itself must never
// scroll. `overflow: hidden` on the body only stops the user from scrolling it: the browser still scrolls the window
// to bring a focused element into view. A hidden checkbox positioned against the window (Settings toggles, before
// `.toggle` got `position: relative`) made a click scroll the window by its offset down the page, which pushed the
// whole app out of sight and left an empty window. Whatever scrolls the window next, put it straight back.

/** The part of `window` this needs (a fake in tests). */
export interface ScrollableWindow {
  readonly scrollX: number;
  readonly scrollY: number;
  scrollTo(x: number, y: number): void;
  addEventListener(type: "scroll", listener: () => void, options?: AddEventListenerOptions): void;
  removeEventListener(type: "scroll", listener: () => void): void;
}

/** Keep the window scrolled to the top left; returns the undo. */
export function pinWindowScroll(win: ScrollableWindow): () => void {
  const pin = () => {
    if (win.scrollX !== 0 || win.scrollY !== 0) win.scrollTo(0, 0);
  };
  win.addEventListener("scroll", pin, { passive: true });
  pin();
  return () => win.removeEventListener("scroll", pin);
}
