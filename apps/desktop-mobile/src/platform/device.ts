export const isAndroid = () => typeof navigator !== "undefined" && /Android/i.test(navigator.userAgent);
export const isIOS = () =>
  typeof navigator !== "undefined" &&
  (/iPad|iPhone|iPod/.test(navigator.userAgent) || (navigator.platform === "MacIntel" && navigator.maxTouchPoints > 1));
export const isMobile = () => isAndroid() || isIOS();
export const isMac = () => typeof navigator !== "undefined" && /Mac/i.test(navigator.platform) && !isIOS();
export const modKey = () => (isMac() || isIOS() ? "⌘" : "Ctrl");
