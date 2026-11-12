import { flushPromises, mount, type VueWrapper } from "@vue/test-utils";
import { createPinia } from "pinia";
import { afterEach, describe, expect, it, vi } from "vitest";
import Toolbar from "../src/components/Toolbar.vue";

const state = {
  tool: "pen",
  scale: 1,
  canUndo: false,
  canRedo: false,
  selectionCount: 0,
  selectionHasText: false,
  selectionTextFree: true,
  selectionTextFont: null,
  pageHasRows: false,
  editingText: false,
  textFormats: [],
  rulerShown: false,
  rulerAngle: 0,
  saveState: "saved",
  saveError: null,
  nativeActive: false,
  penSeen: false,
};

let wrapper: VueWrapper | null = null;
afterEach(() => wrapper?.unmount());

/** A toolbar whose strip reports the given widths, since the test DOM does no layout. */
async function toolbar(clientWidth: number, scrollWidth: number) {
  wrapper = mount(Toolbar, { props: { state: state as never }, global: { plugins: [createPinia()] }, attachTo: document.body });
  const root = wrapper.find<HTMLElement>('[role="toolbar"]');
  const strip = wrapper.find<HTMLElement>(".overflow-x-auto").element;
  Object.defineProperty(strip, "clientWidth", { value: clientWidth, configurable: true });
  Object.defineProperty(strip, "scrollWidth", { value: scrollWidth, configurable: true });
  strip.scrollLeft = 0;
  strip.dispatchEvent(new Event("scroll"));
  await flushPromises();
  return { root, strip };
}

const arrow = (side: string) => wrapper!.find<HTMLButtonElement>(`[aria-label="Scroll toolbar ${side}"]`);

describe("toolbar scrolling", () => {
  it("has no arrows when everything fits", async () => {
    await toolbar(800, 800);
    expect(arrow("left").exists()).toBe(false);
    expect(arrow("right").exists()).toBe(false);
  });

  it("shows arrows on both sides when it overflows, disabling the way it can't go", async () => {
    await toolbar(300, 900);
    expect(arrow("left").element.disabled).toBe(true);
    expect(arrow("right").element.disabled).toBe(false);
  });

  it("scrolls from the arrow without taking focus", async () => {
    const { strip } = await toolbar(300, 900);
    strip.scrollBy = vi.fn();
    const press = new MouseEvent("mousedown", { bubbles: true, cancelable: true });
    arrow("right").element.dispatchEvent(press);
    expect(press.defaultPrevented).toBe(true);
    await arrow("right").trigger("click");
    expect(strip.scrollBy).toHaveBeenCalledWith({ left: 210, behavior: "smooth" });
  });

  it("turns the wheel into sideways movement", async () => {
    const { root, strip } = await toolbar(300, 900);
    const wheel = new WheelEvent("wheel", { deltaY: 100, bubbles: true, cancelable: true });
    root.element.dispatchEvent(wheel);
    expect(strip.scrollLeft).toBe(100);
    expect(wheel.defaultPrevented).toBe(true);
  });

  it("leaves Ctrl+wheel and sideways swipes alone", async () => {
    const { root, strip } = await toolbar(300, 900);
    const zoom = new WheelEvent("wheel", { deltaY: 100, bubbles: true, cancelable: true });
    // happy-dom's WheelEvent doesn't take modifier keys
    Object.defineProperty(zoom, "ctrlKey", { value: true });
    const swipe = new WheelEvent("wheel", { deltaX: 80, deltaY: 2, bubbles: true, cancelable: true });
    root.element.dispatchEvent(zoom);
    root.element.dispatchEvent(swipe);
    expect(strip.scrollLeft).toBe(0);
    expect(zoom.defaultPrevented || swipe.defaultPrevented).toBe(false);
  });

  it("follows a middle-button drag until release, and stops the browser's autoscroll", async () => {
    const { root, strip } = await toolbar(300, 900);
    root.element.setPointerCapture = vi.fn();
    const pointer = (type: string, clientX: number, button = 1) => root.element.dispatchEvent(new PointerEvent(type, { pointerId: 1, pointerType: "mouse", button, clientX, bubbles: true }));
    const press = new MouseEvent("mousedown", { button: 1, bubbles: true, cancelable: true });
    root.element.dispatchEvent(press);
    expect(press.defaultPrevented).toBe(true);

    pointer("pointerdown", 200);
    expect(root.element.setPointerCapture).toHaveBeenCalledWith(1);
    pointer("pointermove", 150);
    expect(strip.scrollLeft).toBe(50);
    pointer("pointerup", 150);
    pointer("pointermove", 20);
    expect(strip.scrollLeft).toBe(50);
  });

  it("ignores a left-button drag", async () => {
    const { root, strip } = await toolbar(300, 900);
    root.element.dispatchEvent(new PointerEvent("pointerdown", { pointerId: 1, pointerType: "mouse", button: 0, clientX: 200, bubbles: true }));
    root.element.dispatchEvent(new PointerEvent("pointermove", { pointerId: 1, pointerType: "mouse", clientX: 100, bubbles: true }));
    expect(strip.scrollLeft).toBe(0);
  });
});
