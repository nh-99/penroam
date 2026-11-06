// Plain-language description of how pen input works on this device, for Settings. It reports what
// the device's path does, not a promise about its quality: see docs/platforms/capabilities.md for
// which devices have been tested by hand.
import { isAndroid, isIOS } from "./device";

export interface InkPath {
  label: string;
  detail: string;
}

const WEB_DETAIL = "Ink is drawn on the web canvas, so how quickly it follows the pen depends on this device's web view.";

export function describeInkPath(nativeKind: string | null, caps: Record<string, unknown> | null): InkPath {
  if (nativeKind === "boox") return { label: "BOOX hardware pen", detail: "The ePaper controller draws ink as you write." };
  if (nativeKind === "ios-pencil") return { label: "Native Apple Pencil layer", detail: "Live ink is drawn natively at the display's refresh rate." };
  if (isIOS()) return { label: "Web canvas", detail: WEB_DETAIL };
  if (isAndroid()) {
    const device = (caps?.device ?? {}) as { manufacturer?: unknown; stylus?: unknown };
    if (device.stylus === true) {
      const samsung = String(device.manufacturer ?? "").toLowerCase() === "samsung";
      return {
        label: samsung ? "S Pen on the web canvas" : "Stylus on the web canvas",
        detail: `Pressure, tilt and hover work where the device reports them, and holding the side button erases. ${WEB_DETAIL}`,
      };
    }
    return { label: "Web canvas", detail: WEB_DETAIL };
  }
  return { label: "Web canvas", detail: "Pens work through the system's tablet driver, with pressure and tilt where it reports them. Holding a pen's side button or using its eraser end erases." };
}
