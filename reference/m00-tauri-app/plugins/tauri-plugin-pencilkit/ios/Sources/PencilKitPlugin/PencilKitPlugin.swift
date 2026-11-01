import InkCore
import PencilInk
import SwiftRs
import Tauri
import UIKit
import WebKit
import WebTuning

/// Embeds a real PKCanvasView (shared PencilInk backend) in the app's UIKit
/// hierarchy, directly above the Tauri WKWebView, at the rectangle the web
/// editor reports.
///
/// Placement: the web UI sends its canvas rect in CSS px relative to the
/// layout viewport (getBoundingClientRect). WKWebView maps CSS px 1:1 to
/// points at page zoom 1, offset by the scroll view's contentOffset (which
/// also carries the safe-area content inset). The rect is converted from
/// web view coordinates into the web view's superview and the canvas is
/// inserted above the web view. The web canvas ignores input while the native
/// canvas is attached, so Pencil samples are never processed twice. Web
/// controls outside the rect keep receiving touches; web content *under* the
/// rect is covered (no native/web overlap inside the editor region).
class PencilKitPlugin: Plugin {
    private weak var webview: WKWebView?
    private var backend: PencilInkBackend?
    private var cssRect = CGRect.zero
    private var observers: [NSObjectProtocol] = []

    private var frameRate: [String: Any] = [:]

    override func load(webview: WKWebView) {
        self.webview = webview
        // 120 Hz: the hook installed in initPlugin() applied the flag when the
        // web view was created (setting it here, after creation, has no
        // effect — measured). Report what happened for diagnostics.
        frameRate = ["hook": WebFrameRate.hookResult, "applied": WebFrameRate.lastHookApply]
        // Re-place after rotation / multitasking resize even if JS is late.
        observers.append(NotificationCenter.default.addObserver(
            forName: UIDevice.orientationDidChangeNotification, object: nil, queue: .main) { [weak self] _ in self?.place() })
    }

    deinit { observers.forEach(NotificationCenter.default.removeObserver) }

    struct Rect: Decodable { let x: Double; let y: Double; let width: Double; let height: Double }
    struct Transform: Decodable { let scale: Double; let offsetX: Double; let offsetY: Double }
    struct AttachArgs: Decodable {
        let x: Double; let y: Double; let width: Double; let height: Double
        let tool: String?; let penWidth: Double?; let strokes: String?; let transform: Transform?
        let fingerDraws: Bool?
    }
    struct ToolArgs: Decodable { let tool: String; let width: Double }
    struct StrokesArgs: Decodable { let strokes: String }
    struct NativeArgs: Decodable { let data: String }
    struct FingerArgs: Decodable { let allowed: Bool }

    // MARK: commands (invoked via the Rust `call` command, same names as Android)

    @objc public func attach(_ invoke: Invoke) throws {
        let a = try invoke.parseArgs(AttachArgs.self)
        DispatchQueue.main.async { [self] in
            let b = backend ?? makeBackend()
            cssRect = CGRect(x: a.x, y: a.y, width: a.width, height: a.height)
            b.setTool(a.tool == "eraser" ? .eraser : .pen, width: a.penWidth ?? 3)
            b.setFingerDrawing(a.fingerDraws ?? false)
            if let s = a.strokes, let d = s.data(using: .utf8), let doc = try? JSONDecoder().decode(InkDocument.self, from: d) {
                b.loadStrokes(doc.strokes)
            }
            place()
            if let t = a.transform { b.setViewport(scale: t.scale, offsetX: t.offsetX, offsetY: t.offsetY) }
            invoke.resolve(["mode": "PENCILKIT", "capabilities": b.getCapabilities(), "status": status()])
        }
    }

    @objc public func detach(_ invoke: Invoke) {
        DispatchQueue.main.async { [self] in
            backend?.dispose(); backend = nil
            invoke.resolve()
        }
    }

    @objc public func setBounds(_ invoke: Invoke) throws {
        let r = try invoke.parseArgs(Rect.self)
        DispatchQueue.main.async { [self] in
            cssRect = CGRect(x: r.x, y: r.y, width: r.width, height: r.height)
            place()
            invoke.resolve()
        }
    }

    @objc public func setBrush(_ invoke: Invoke) throws {
        let a = try invoke.parseArgs(ToolArgs.self)
        DispatchQueue.main.async { [self] in
            backend?.setTool(a.tool == "eraser" ? .eraser : .pen, width: a.width)
            invoke.resolve()
        }
    }

    @objc public func setFingerDrawing(_ invoke: Invoke) throws {
        let a = try invoke.parseArgs(FingerArgs.self)
        DispatchQueue.main.async { [self] in backend?.setFingerDrawing(a.allowed); invoke.resolve() }
    }

    @objc public func setTransform(_ invoke: Invoke) throws {
        let t = try invoke.parseArgs(Transform.self)
        DispatchQueue.main.async { [self] in
            backend?.setViewport(scale: t.scale, offsetX: t.offsetX, offsetY: t.offsetY)
            invoke.resolve()
        }
    }

    @objc public func clear(_ invoke: Invoke) {
        DispatchQueue.main.async { [self] in backend?.clear(); invoke.resolve() }
    }

    @objc public func loadStrokes(_ invoke: Invoke) throws {
        let a = try invoke.parseArgs(StrokesArgs.self)
        DispatchQueue.main.async { [self] in
            guard let b = backend else { return invoke.reject("not attached") }
            do {
                let doc = try JSONDecoder().decode(InkDocument.self, from: Data(a.strokes.utf8))
                invoke.resolve(["ms": b.loadStrokes(doc.strokes), "count": doc.strokes.count])
            } catch { invoke.reject("\(error)") }
        }
    }

    @objc public func exportStrokes(_ invoke: Invoke) {
        DispatchQueue.main.async { [self] in
            guard let b = backend else { return invoke.reject("not attached") }
            let doc = InkDocument(strokes: b.exportStrokes(), meta: ["app": .string("tauri-pencilkit")])
            invoke.resolve(["strokes": String(decoding: (try? JSONEncoder().encode(doc)) ?? Data(), as: UTF8.self)])
        }
    }

    @objc public func exportNative(_ invoke: Invoke) {
        DispatchQueue.main.async { [self] in
            guard let b = backend else { return invoke.reject("not attached") }
            invoke.resolve(["data": b.nativeDrawingData().base64EncodedString()])
        }
    }

    @objc public func loadNative(_ invoke: Invoke) throws {
        let a = try invoke.parseArgs(NativeArgs.self)
        DispatchQueue.main.async { [self] in
            guard let b = backend, let d = Data(base64Encoded: a.data) else { return invoke.reject("not attached or bad data") }
            do { try b.loadNativeDrawing(d); invoke.resolve() } catch { invoke.reject("\(error)") }
        }
    }

    @objc public func getCapabilities(_ invoke: Invoke) {
        DispatchQueue.main.async { [self] in
            invoke.resolve((backend ?? PencilInkBackend()).getCapabilities())
        }
    }

    @objc public func getMetrics(_ invoke: Invoke) {
        DispatchQueue.main.async { [self] in
            guard let b = backend else { return invoke.reject("not attached") }
            var m = b.getMetrics()
            m["placement"] = status()
            invoke.resolve(m)
        }
    }

    // MARK: embedding

    private func makeBackend() -> PencilInkBackend {
        let b = PencilInkBackend()
        b.onStrokesCommitted = { [weak self] strokes in
            let json = String(decoding: (try? JSONEncoder().encode(strokes)) ?? Data(), as: UTF8.self)
            self?.trigger("strokes", data: ["strokes": json])
        }
        b.onStrokesErased = { [weak self] ids in self?.trigger("erased", data: ["ids": ids as JSArray]) }
        b.onViewportChanged = { [weak self] s, x, y in
            self?.trigger("viewport", data: ["scale": s, "offsetX": x, "offsetY": y])
        }
        backend = b
        return b
    }

    /// CSS rect -> frame in the web view's superview; canvas above the web view.
    private func place() {
        guard let b = backend, let wv = webview, let host = wv.superview else { return }
        let sv = wv.scrollView
        let z = sv.zoomScale
        let inWebView = CGRect(x: cssRect.minX * z - sv.contentOffset.x, y: cssRect.minY * z - sv.contentOffset.y,
                               width: cssRect.width * z, height: cssRect.height * z)
        let frame = wv.convert(inWebView, to: host)
        if b.canvas.superview !== host {
            host.insertSubview(b.canvas, aboveSubview: wv)
            b.attachSurface(to: host, frame: frame)
        } else {
            b.setFrame(frame)
        }
        trigger("status", data: status().compactMapValues { $0 as? JSValue })
    }

    private func status() -> [String: Any] {
        guard let wv = webview, let b = backend else { return ["attached": false] }
        return ["attached": b.canvas.superview != nil,
                "frame": NSCoder.string(for: b.canvas.frame),
                "cssRect": NSCoder.string(for: cssRect),
                "webviewFrame": NSCoder.string(for: wv.frame),
                "contentOffset": NSCoder.string(for: wv.scrollView.contentOffset),
                "adjustedInset": NSCoder.string(for: wv.scrollView.adjustedContentInset),
                "zoomScale": Double(wv.scrollView.zoomScale),
                "safeArea": NSCoder.string(for: wv.safeAreaInsets),
                "strokes": b.strokeCount(),
                "webFrameRate": frameRate]
    }
}

@_cdecl("init_plugin_pencilkit")
func initPlugin() -> Plugin {
    // Runs during plugin registration, before Tauri creates the WKWebView, so
    // the -[WKWebView initWithFrame:configuration:] hook lifts WebKit's 60 fps
    // rendering-update cap for the app's web view (Canvas mode benefits too).
    // Private SPI — App Store review risk (docs/status.md).
    WebFrameRate.installHighRefreshHook()
    return PencilKitPlugin()
}
