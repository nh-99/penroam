// Sideways scrolling for a bar with no visible scrollbar: tracks whether its contents overflow, for
// the arrow buttons, and moves it from those arrows, the mouse wheel or a middle-button drag.
import { onBeforeUnmount, onMounted, onUpdated, ref, type Ref } from "vue";
import { tabsScroll, type TabsScroll } from "../components/ui/tabsScroll";

/** What a bar scrolled by [`useSideScroll`] binds to its template. */
export interface SideScroll {
  /** Whether the contents overflow, and which ways they can still move. */
  scroll: Ref<TabsScroll>;
  /** Re-reads the bar's position. Bind to its `scroll` event. */
  measure: () => void;
  /** Moves the bar one step left (-1) or right (1). For the arrow buttons. */
  nudge: (direction: -1 | 1) => void;
  /** Wheel and middle-button listeners, for `v-on` on the bar or an element around it. */
  handlers: {
    wheel: (e: WheelEvent) => void;
    mousedown: (e: MouseEvent) => void;
    pointerdown: (e: PointerEvent) => void;
    pointermove: (e: PointerEvent) => void;
    pointerup: (e: PointerEvent) => void;
    pointercancel: (e: PointerEvent) => void;
  };
}

/**
 * Scrolling state and input for the sideways-scrolling element in `strip`.
 *
 * Call from a component's `setup`. The element is measured again whenever it resizes or the
 * component re-renders, so contents that come and go keep the arrows correct.
 */
export function useSideScroll(strip: Ref<HTMLElement | null>): SideScroll {
  const scroll = ref<TabsScroll>({ overflowing: false, canLeft: false, canRight: false });
  let observer: ResizeObserver | null = null;
  let drag: { pointerId: number; startX: number; startLeft: number } | null = null;

  function measure() {
    const el = strip.value;
    if (!el) return;
    const next = tabsScroll(el.scrollLeft, el.clientWidth, el.scrollWidth);
    const now = scroll.value;
    // assigning only on a change matters: this runs after every render, and a write would cause another
    if (next.overflowing !== now.overflowing || next.canLeft !== now.canLeft || next.canRight !== now.canRight) scroll.value = next;
  }

  /** Moves most of a bar's width, so the item at the edge stays in view for context. */
  function nudge(direction: -1 | 1) {
    const el = strip.value;
    // an animated scroll is a run of partial redraws on an e-paper screen
    const instant = document.documentElement.dataset.theme === "epaper";
    el?.scrollBy({ left: direction * el.clientWidth * 0.7, behavior: instant ? "auto" : "smooth" });
  }

  function onWheel(e: WheelEvent) {
    const el = strip.value;
    // Ctrl+wheel is zoom, and a sideways swipe already scrolls the bar by itself
    if (!el || !scroll.value.overflowing || e.ctrlKey || Math.abs(e.deltaX) > Math.abs(e.deltaY)) return;
    e.preventDefault();
    el.scrollLeft += e.deltaY;
  }

  function onMouseDown(e: MouseEvent) {
    // stops the browser's own middle-button autoscroll, and keeps focus in a text box being edited
    if (e.button === 1) e.preventDefault();
  }

  function onPointerDown(e: PointerEvent) {
    const el = strip.value;
    if (e.pointerType !== "mouse" || e.button !== 1 || !el || !scroll.value.overflowing) return;
    drag = { pointerId: e.pointerId, startX: e.clientX, startLeft: el.scrollLeft };
    // captured so the drag carries on when the mouse leaves the bar, which is only a few rows tall
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
  }

  function onPointerMove(e: PointerEvent) {
    const el = strip.value;
    if (el && drag?.pointerId === e.pointerId) el.scrollLeft = drag.startLeft - (e.clientX - drag.startX);
  }

  function onPointerEnd(e: PointerEvent) {
    if (drag?.pointerId === e.pointerId) drag = null;
  }

  onMounted(() => {
    measure();
    if (typeof ResizeObserver !== "undefined" && strip.value) {
      observer = new ResizeObserver(measure);
      observer.observe(strip.value);
    }
  });
  // what's in the bar changes without its own size changing, which the observer wouldn't see
  onUpdated(measure);
  onBeforeUnmount(() => observer?.disconnect());

  return {
    scroll,
    measure,
    nudge,
    handlers: { wheel: onWheel, mousedown: onMouseDown, pointerdown: onPointerDown, pointermove: onPointerMove, pointerup: onPointerEnd, pointercancel: onPointerEnd },
  };
}
