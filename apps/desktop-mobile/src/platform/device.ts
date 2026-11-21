export const isAndroid = () => typeof navigator !== "undefined" && /Android/i.test(navigator.userAgent);
export const isIOS = () =>
  typeof navigator !== "undefined" &&
  (/iPad|iPhone|iPod/.test(navigator.userAgent) || (navigator.platform === "MacIntel" && navigator.maxTouchPoints > 1));
export const isMobile = () => isAndroid() || isIOS();
/** A phone rather than a tablet: a mobile device whose screen is narrower than 600 CSS px both ways round. */
export const isPhone = () => isMobile() && typeof screen !== "undefined" && Math.min(screen.width, screen.height) < 600;
export const isMac = () => typeof navigator !== "undefined" && /Mac/i.test(navigator.platform) && !isIOS();
export const modKey = () => (isMac() || isIOS() ? "⌘" : "Ctrl");
