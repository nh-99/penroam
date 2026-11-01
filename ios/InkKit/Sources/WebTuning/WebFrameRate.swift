#if canImport(WebKit) && canImport(UIKit)
import Foundation
import WebKit

/// Lifts WKWebView's 60 Hz cap on rendering updates (requestAnimationFrame)
/// on ProMotion displays.
///
/// WebKit throttles page rendering updates to ~60 fps by default via the
/// internal feature "PreferPageRenderingUpdatesNear60FPSEnabled" (exposed to
/// users only as a Safari Feature Flag). There is no public API, so this uses
/// the same SPI Safari's feature-flag UI uses:
///   +[WKPreferences _features] -> [_WKFeature] (has `key`)
///   -[WKPreferences _setEnabled:forFeature:]
/// Every step is guarded with respondsToSelector; if WebKit changes, this
/// becomes a no-op and reports why. **Private API: App Store review risk.**
public enum WebFrameRate {
    public static let featureKey = "PreferPageRenderingUpdatesNear60FPSEnabled"

    /// Returns a diagnostic dictionary; `applied` tells whether the flag was set.
    @discardableResult
    public static func allowHighRefresh(_ prefs: WKPreferences, enabled: Bool = true) -> [String: Any] {
        let featuresSel = NSSelectorFromString("_features")
        let setSel = NSSelectorFromString("_setEnabled:forFeature:")
        guard (WKPreferences.self as AnyObject).responds(to: featuresSel) else {
            return ["applied": false, "reason": "+[WKPreferences _features] unavailable"]
        }
        guard prefs.responds(to: setSel) else {
            return ["applied": false, "reason": "-[WKPreferences _setEnabled:forFeature:] unavailable"]
        }
        guard let list = (WKPreferences.self as AnyObject).perform(featuresSel)?.takeUnretainedValue() as? [NSObject] else {
            return ["applied": false, "reason": "_features returned nothing"]
        }
        guard let feature = list.first(where: { ($0.value(forKey: "key") as? String) == featureKey }) else {
            return ["applied": false, "reason": "feature \(featureKey) not found", "featureCount": list.count]
        }
        typealias SetFn = @convention(c) (AnyObject, Selector, Bool, AnyObject) -> Void
        let imp = prefs.method(for: setSel)
        // enabled=true for "allow high refresh" means *disabling* the 60 fps preference.
        unsafeBitCast(imp, to: SetFn.self)(prefs, setSel, !enabled, feature)
        return ["applied": true, "feature": featureKey, "preferNear60fps": !enabled]
    }

    private static var hookInstalled = false
    public private(set) static var hookResult: [String: Any] = [:]

    /// For hosts that cannot touch the WKWebViewConfiguration before the web
    /// view exists (Tauri: plugins only receive the finished WKWebView).
    /// Hooks -[WKWebView initWithFrame:configuration:] and applies the flag to
    /// the configuration's preferences *before* the original initializer runs,
    /// i.e. the same timing as setting it on the configuration by hand.
    ///
    /// History (measured on iPad13,4 / iPadOS 26.6.2): a first version hooked
    /// -[WKPreferences init]; it never fired, because WKWebViewConfiguration
    /// creates its WKPreferences wrapper internally (API object wrapper)
    /// without calling -init. Setting the flag after creation has no effect.
    /// Idempotent. Subclasses (wry's WKWebView subclass) reach this through super.
    @discardableResult
    public static func installHighRefreshHook() -> [String: Any] {
        if hookInstalled { return hookResult }
        hookInstalled = true
        let sel = NSSelectorFromString("initWithFrame:configuration:")
        guard let m = class_getInstanceMethod(WKWebView.self, sel) else {
            hookResult = ["installed": false, "reason": "no -[WKWebView initWithFrame:configuration:]"]; return hookResult
        }
        typealias InitFn = @convention(c) (AnyObject, Selector, CGRect, WKWebViewConfiguration) -> Unmanaged<AnyObject>?
        let original = unsafeBitCast(method_getImplementation(m), to: InitFn.self)
        // `init` consumes self and returns +1: forward the same pointer and
        // pass the result through untouched so ownership stays balanced.
        let block: @convention(block) (AnyObject, CGRect, WKWebViewConfiguration) -> Unmanaged<AnyObject>? = { obj, frame, cfg in
            var r = allowHighRefresh(cfg.preferences)
            r["hookCalls"] = ((lastHookApply["hookCalls"] as? Int) ?? 0) + 1
            lastHookApply = r
            return original(obj, sel, frame, cfg)
        }
        method_setImplementation(m, imp_implementationWithBlock(block))
        hookResult = ["installed": true, "hook": "-[WKWebView initWithFrame:configuration:]"]
        return hookResult
    }

    public private(set) static var lastHookApply: [String: Any] = [:]
}
#endif
