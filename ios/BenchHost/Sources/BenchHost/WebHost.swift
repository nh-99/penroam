#if canImport(UIKit)
import UIKit
import WebKit
import WebTuning

/// Runs the *unmodified Tauri frontend bundle* (tauri_app/dist) in WKWebView.
/// Tauri on iOS is WKWebView + the same JS served through a custom URL scheme,
/// so this measures the same Pointer Events / Canvas 2D path. It is a proxy
/// for the Tauri iPad build (which needs Xcode), not the Tauri app itself:
/// the Rust backend is replaced by a small Swift bridge with the same command
/// names (save_strokes, list_strokes, load_strokes, write_perf, system_info,
/// proc_stats, startup_ms, autobench_requested).
final class WebHostView: UIView, WKScriptMessageHandlerWithReply, WKURLSchemeHandler {
    static var autobench = false
    static var frameRateResult: [String: Any] = [:]
    private var web: WKWebView!
    private let root: URL? = Bundle.module.url(forResource: "web", withExtension: nil)

    override init(frame: CGRect) {
        super.init(frame: frame)
        backgroundColor = .white
        //   hook        — WebFrameRate.installHighRefreshHook() before creating the
        //                 configuration (the mechanism the Tauri plugin uses)
        if (UserDefaults.standard.string(forKey: "bench.web120mode") ?? "early") == "hook" { WebFrameRate.installHighRefreshHook() }
        let cfg = WKWebViewConfiguration()
        // 120 Hz experiment (UserDefaults "bench.web120mode"):
        //   off   — WebKit default (~60 fps rendering updates)
        //   early — flag set on the configuration before the WKWebView exists
        //   late  — flag set on webview.configuration.preferences after creation
        //           (the only option a Tauri plugin has: load(webview:) runs after creation)
        let mode = UserDefaults.standard.string(forKey: "bench.web120mode") ?? "early"
        //   defaults    — only UserDefaults "WebKit<Feature>" = false (no SPI on the config)
        //   late-reload — like late, then reload the page
        if mode == "defaults" {
            UserDefaults.standard.set(false, forKey: "WebKit" + WebFrameRate.featureKey)
        } else {
            UserDefaults.standard.removeObject(forKey: "WebKit" + WebFrameRate.featureKey)
        }
        if mode == "early" { WebHostView.frameRateResult = WebFrameRate.allowHighRefresh(cfg.preferences) }
        else { WebHostView.frameRateResult = ["applied": false, "reason": "mode \(mode)"] }
        WebHostView.frameRateResult["mode"] = mode
        if mode == "hook" { WebHostView.frameRateResult = ["mode": mode, "hook": WebFrameRate.hookResult, "lastApply": WebFrameRate.lastHookApply] }
        cfg.setURLSchemeHandler(self, forURLScheme: "m00")
        cfg.userContentController.addScriptMessageHandler(self, contentWorld: .page, name: "m00")
        cfg.userContentController.addUserScript(WKUserScript(
            source: "window.__M00_HOST__ = 'ios-wkwebview';", injectionTime: .atDocumentStart, forMainFrameOnly: true))
        web = WKWebView(frame: bounds, configuration: cfg)
        web.autoresizingMask = [.flexibleWidth, .flexibleHeight]
        if mode == "hook" { WebHostView.frameRateResult = ["mode": mode, "hook": WebFrameRate.hookResult, "lastApply": WebFrameRate.lastHookApply] }
        if mode == "late" || mode == "late-reload" {
            WebHostView.frameRateResult = WebFrameRate.allowHighRefresh(web.configuration.preferences)
            WebHostView.frameRateResult["mode"] = mode
            WebHostView.frameRateResult["samePreferencesObject"] = web.configuration.preferences === cfg.preferences
        }
        NSLog("BENCHWEB|frameRate %@", "\(WebHostView.frameRateResult)")
        web.scrollView.isScrollEnabled = false
        web.scrollView.bounces = false
        web.scrollView.contentInsetAdjustmentBehavior = .never
        if #available(iOS 16.4, *) { web.isInspectable = true }
        addSubview(web)
        if root?.appendingPathComponent("index.html") != nil, FileManager.default.fileExists(atPath: root!.appendingPathComponent("index.html").path) {
            web.load(URLRequest(url: URL(string: "m00://app/index.html")!))
            if mode == "late-reload" {
                DispatchQueue.main.asyncAfter(deadline: .now() + 1) { [weak self] in self?.web.reload() }
            }
        } else {
            web.loadHTMLString("<h2 style='font-family:sans-serif'>web bundle missing — run ios/BenchHost/tools/build_web.sh</h2>", baseURL: nil)
        }
    }

    required init?(coder: NSCoder) { fatalError() }

    /// Reloads the page with the one-shot autobench flag set; the Tauri
    /// frontend then runs its Workload D and calls write_perf + exit_app.
    func runAutobench() {
        WebHostView.autobench = true
        web.reload()
    }

    /// Last status line the page reported via exit_app / write_perf.
    static var lastEvent = "idle"

    // MARK: m00:// scheme (serves the bundled dist/; the scheme, the "m00" handler
    // and __M00_HOST__ are the frozen prototype bundle's bridge contract)

    func webView(_ webView: WKWebView, start task: WKURLSchemeTask) {
        guard let url = task.request.url, let root else { task.didFailWithError(URLError(.fileDoesNotExist)); return }
        var path = url.path.isEmpty || url.path == "/" ? "/index.html" : url.path
        if path.contains("..") { path = "/index.html" }
        let file = root.appendingPathComponent(String(path.dropFirst()))
        guard let data = try? Data(contentsOf: file) else { task.didFailWithError(URLError(.fileDoesNotExist)); return }
        let mime: String = switch file.pathExtension {
        case "html": "text/html"
        case "js", "mjs": "text/javascript"
        case "css": "text/css"
        case "json": "application/json"
        case "svg": "image/svg+xml"
        case "png": "image/png"
        default: "application/octet-stream"
        }
        task.didReceive(HTTPURLResponse(url: url, statusCode: 200, httpVersion: "HTTP/1.1",
                                        headerFields: ["Content-Type": mime, "Content-Length": "\(data.count)"])!)
        task.didReceive(data)
        task.didFinish()
    }

    func webView(_ webView: WKWebView, stop task: WKURLSchemeTask) {}

    // MARK: command bridge

    func userContentController(_ ucc: WKUserContentController, didReceive message: WKScriptMessage,
                               replyHandler: @escaping (Any?, String?) -> Void) {
        guard let body = message.body as? [String: Any], let cmd = body["cmd"] as? String else {
            return replyHandler(nil, "bad message")
        }
        let args = body["args"] as? [String: Any] ?? [:]
        do {
            switch cmd {
            case "save_strokes":
                replyHandler(try Export.save(dir: "strokes", name: "strokes-\(Export.stamp()).json", text: args["json"] as? String ?? "", log: false), nil)
            case "list_strokes":
                replyHandler(Export.list(dir: "strokes"), nil)
            case "load_strokes":
                replyHandler(try Export.load(dir: "strokes", name: args["name"] as? String ?? ""), nil)
            case "write_perf":
                let label = (args["label"] as? String ?? "perf").filter { $0.isLetter || $0.isNumber || $0 == "-" }
                let path = try Export.save(dir: "perf", name: "perf-ios-wkwebview-\(label)-\(Export.stamp()).json", text: args["json"] as? String ?? "", log: true)
                WebHostView.lastEvent = "wrote \(path)"
                replyHandler(path, nil)
            case "system_info":
                var info = Export.systemInfo(extra: ["webview": "WKWebView", "host": "BenchHost (Tauri frontend proxy)"])
                info["webFrameRate"] = WebHostView.frameRateResult
                replyHandler(info, nil)
            case "proc_stats":
                replyHandler(Export.procStats(note: "app process only; WKWebView WebContent process is separate and not included"), nil)
            case "startup_ms":
                replyHandler(Export.processAgeMs(), nil)
            case "autobench_requested":
                // One-shot: true only for the autorun sequence (see RootViewController).
                replyHandler(WebHostView.autobench, nil)
                WebHostView.autobench = false
            case "exit_app":
                NSLog("BENCHAUTORUN|web-done")
                WebHostView.lastEvent = "web-bench-done"
                replyHandler(nil, nil)
            default:
                replyHandler(nil, "unknown command \(cmd)")
            }
        } catch {
            replyHandler(nil, "\(cmd): \(error)")
        }
    }
}
#endif
