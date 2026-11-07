// Which way a sideways-scrolling tab bar can still move. Separate from the component so the rounding
// can be tested: on high-density screens scroll positions are fractional, and the far end can sit
// a fraction of a pixel short of `scrollWidth - clientWidth`.
export interface TabsScroll {
  /** The tabs don't fit, so the arrows are shown. */
  overflowing: boolean;
  canLeft: boolean;
  canRight: boolean;
}

export function tabsScroll(scrollLeft: number, clientWidth: number, scrollWidth: number): TabsScroll {
  const hidden = scrollWidth - clientWidth;
  return { overflowing: hidden > 1, canLeft: scrollLeft > 1, canRight: scrollLeft < hidden - 1 };
}
