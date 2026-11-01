// Penroam NativeInkSurface for iPadOS (M01.6): a UIKit Pencil preview layer
// above the Tauri WKWebView.
//
// Why: WKWebView renders rAF at 60 Hz by default (measured in M00 on an
// iPad Pro M1, 120 Hz display) while UIKit renders at 120 Hz. Live Pencil
// strokes are therefore drawn natively; Vue/Rust keep document semantics.
//
//  * A UIGestureRecognizer on the web view with allowedTouchTypes = [.pencil]
//    claims Pencil touches inside the editor rect (cancelsTouchesInView), so
//    the web canvas never sees them twice; fingers still reach the web view
//    (pan/zoom/UI). Palm contacts are fingers, so they never draw.
//  * Coalesced touches are recorded; predicted touches are drawn only on the
//    preview; estimated force updates patch recorded points.
//  * On pen-up the stroke is sent to the web editor ("stroke" event); it stays
//    on the preview until the editor calls `commitAck` after painting its
//    committed copy (HandoffQueue; 2 s timeout as a safety net).
//
// Status: implemented and compiled against the iOS SDK only where a macOS +
// Xcode toolchain is available; see docs/platforms/ipad.md for what has and
// has not been validated on hardware.
#if canImport(UIKit)
import InkCore
import PencilSurface
import SwiftRs
import Tauri
import UIKit
import WebKit
import WebTuning

final class PencilPreviewView: UIView {
    var live: LiveStroke?
    /// finished strokes awaiting the web editor's commit acknowledgement
    var pending: [(id: String, points: [InkPoint], brush: Brush)] = []
    var transform2 = ViewTransform()

    override init(frame: CGRect) {
        super.init(frame: frame)
        isOpaque = false
        backgroundColor = .clear
        isUserInteractionEnabled = false
        contentMode = .redraw
    }
    required init?(coder: NSCoder) { fatalError() }

    private func color(_ hex: String, _ alpha: Double) -> UIColor {
        var v: UInt64 = 0
        Scanner(string: String(hex.dropFirst())).scanHexInt64(&v)
        return UIColor(red: CGFloat((v >> 16) & 0xff) / 255, green: CGFloat((v >> 8) & 0xff) / 255, blue: CGFloat(v & 0xff) / 255, alpha: CGFloat(alpha))
    }

    private func draw(_ ctx: CGContext, _ pts: [InkPoint], _ b: Brush, alpha: Double) {
        guard !pts.isEmpty else { return }
        let t = transform2
        ctx.saveGState()
        ctx.translateBy(x: CGFloat(t.offsetX), y: CGFloat(t.offsetY))
        ctx.scaleBy(x: CGFloat(t.scale), y: CGFloat(t.scale))
        ctx.setLineCap(.round); ctx.setLineJoin(.round)
        let c = color(b.color, b.tool == "highlighter" ? b.opacity * alpha : alpha)
        ctx.setStrokeColor(c.cgColor); ctx.setFillColor(c.cgColor)
        let pe = b.pressure && b.tool == "pen"
        for piece in Geometry.pieces(pts, b.width, pe) {
            switch piece {
            case let .dot(x, y, w):
                ctx.fillEllipse(in: CGRect(x: x - w / 2, y: y - w / 2, width: w, height: w))
            case let .line(x0, y0, x1, y1, w):
                ctx.setLineWidth(CGFloat(w)); ctx.move(to: CGPoint(x: x0, y: y0)); ctx.addLine(to: CGPoint(x: x1, y: y1)); ctx.strokePath()
            case let .quad(x0, y0, cx, cy, x1, y1, w):
                ctx.setLineWidth(CGFloat(w)); ctx.move(to: CGPoint(x: x0, y: y0))
                ctx.addQuadCurve(to: CGPoint(x: x1, y: y1), control: CGPoint(x: cx, y: cy)); ctx.strokePath()
            }
        }
        ctx.restoreGState()
    }

    override func draw(_ rect: CGRect) {
        guard let ctx = UIGraphicsGetCurrentContext() else { return }
        for p in pending { draw(ctx, p.points, p.brush, alpha: 1) }
        if let l = live {
            draw(ctx, l.points, l.brush, alpha: 1)
            if let last = l.points.last, !l.predicted.isEmpty { draw(ctx, [last] + l.predicted, l.brush, alpha: 0.5) }
        }
    }
}

final class PencilCapture: UIGestureRecognizer {
    weak var plugin: NativeInkPlugin?
    override func touchesBegan(_ touches: Set<UITouch>, with event: UIEvent) {
        guard let t = touches.first, plugin?.begin(t, event) == true else { state = .failed; return }
        state = .began
    }
    override func touchesMoved(_ touches: Set<UITouch>, with event: UIEvent) {
        guard let t = touches.first else { return }
        plugin?.move(t, event)
        state = .changed
    }
    override func touchesEnded(_ touches: Set<UITouch>, with event: UIEvent) {
        if let t = touches.first { plugin?.end(t, event, cancelled: false) }
        state = .ended
    }
    override func touchesCancelled(_ touches: Set<UITouch>, with event: UIEvent) {
        if let t = touches.first { plugin?.end(t, event, cancelled: true) }
        state = .cancelled
    }
    override func touchesEstimatedPropertiesUpdated(_ touches: Set<UITouch>) {
        for t in touches { plugin?.estimated(t) }
    }
}

class NativeInkPlugin: Plugin, UIGestureRecognizerDelegate {
    private weak var webview: WKWebView?
    private var preview: PencilPreviewView?
    private var capture: PencilCapture?
    private var cssRect = CGRect.zero
    private var brush = Brush()
    private var transform = ViewTransform()
    private var visible = false
    private let handoff = HandoffQueue()
    private var metrics: [String: Double] = [:]
    private var expireTimer: Timer?

    override func load(webview: WKWebView) {
        self.webview = webview
    }

    struct AttachArgs: Decodable {
        let x: Double; let y: Double; let width: Double; let height: Double
        let tool: String?; let color: String?; let penWidth: Double?; let pressure: Bool?; let opacity: Double?
        let transform: T?
        struct T: Decodable { let scale: Double; let offsetX: Double; let offsetY: Double }
    }
    struct RectArgs: Decodable { let x: Double; let y: Double; let width: Double; let height: Double }
    struct BrushArgs: Decodable { let tool: String; let color: String?; let penWidth: Double?; let pressure: Bool?; let opacity: Double? }
    struct TransformArgs: Decodable { let scale: Double; let offsetX: Double; let offsetY: Double }
    struct VisibleArgs: Decodable { let visible: Bool }
    struct AckArgs: Decodable { let id: String }

    @objc public func getCapabilities(_ invoke: Invoke) {
        DispatchQueue.main.async {
            invoke.resolve([
                "pencilSurface": UIDevice.current.userInterfaceIdiom == .pad,
                "maximumFramesPerSecond": UIScreen.main.maximumFramesPerSecond,
                "webFrameRate": WebFrameRate.lastHookApply as Any,
            ])
        }
    }

    @objc public func attach(_ invoke: Invoke) throws {
        let a = try invoke.parseArgs(AttachArgs.self)
        DispatchQueue.main.async { [self] in
            guard let wv = webview, let host = wv.superview else { return invoke.reject("web view not ready") }
            cssRect = CGRect(x: a.x, y: a.y, width: a.width, height: a.height)
            brush = Brush(tool: a.tool ?? "pen", color: a.color ?? "#111111", width: a.penWidth ?? 2.2, pressure: a.pressure ?? true, opacity: a.opacity ?? 1)
            if let t = a.transform { transform = ViewTransform(scale: t.scale, offsetX: t.offsetX, offsetY: t.offsetY) }
            let p = preview ?? PencilPreviewView(frame: .zero)
            if p.superview !== host { host.insertSubview(p, aboveSubview: wv) }
            preview = p
            if capture == nil {
                let g = PencilCapture(target: nil, action: nil)
                g.plugin = self
                g.allowedTouchTypes = [NSNumber(value: UITouch.TouchType.pencil.rawValue)]
                g.cancelsTouchesInView = true
                g.delaysTouchesBegan = false
                g.delegate = self
                wv.addGestureRecognizer(g)
                capture = g
            }
            place()
            setShown(false)
            expireTimer = Timer.scheduledTimer(withTimeInterval: 0.5, repeats: true) { [weak self] _ in self?.expire() }
            invoke.resolve(["mode": "UIKIT_PENCIL"])
        }
    }

    @objc public func detach(_ invoke: Invoke) {
        DispatchQueue.main.async { [self] in
            expireTimer?.invalidate(); expireTimer = nil
            if let g = capture { g.view?.removeGestureRecognizer(g) }
            capture = nil
            preview?.removeFromSuperview(); preview = nil
            invoke.resolve()
        }
    }

    @objc public func setBounds(_ invoke: Invoke) throws {
        let r = try invoke.parseArgs(RectArgs.self)
        DispatchQueue.main.async { [self] in cssRect = CGRect(x: r.x, y: r.y, width: r.width, height: r.height); place(); invoke.resolve() }
    }

    @objc public func setBrush(_ invoke: Invoke) throws {
        let b = try invoke.parseArgs(BrushArgs.self)
        DispatchQueue.main.async { [self] in
            brush = Brush(tool: b.tool, color: b.color ?? brush.color, width: b.penWidth ?? brush.width, pressure: b.pressure ?? brush.pressure, opacity: b.opacity ?? brush.opacity)
            invoke.resolve()
        }
    }

    @objc public func setTransform(_ invoke: Invoke) throws {
        let t = try invoke.parseArgs(TransformArgs.self)
        DispatchQueue.main.async { [self] in
            transform = ViewTransform(scale: t.scale, offsetX: t.offsetX, offsetY: t.offsetY)
            preview?.transform2 = transform
            preview?.setNeedsDisplay()
            invoke.resolve()
        }
    }

    @objc public func setVisible(_ invoke: Invoke) throws {
        let v = try invoke.parseArgs(VisibleArgs.self)
        DispatchQueue.main.async { [self] in setShown(v.visible); invoke.resolve() }
    }

    @objc public func commitAck(_ invoke: Invoke) throws {
        let a = try invoke.parseArgs(AckArgs.self)
        DispatchQueue.main.async { [self] in
            if handoff.ack(a.id) {
                preview?.pending.removeAll { $0.id == a.id }
                preview?.setNeedsDisplay()
            }
            invoke.resolve()
        }
    }

    // The iOS surface is preview-only: committed ink is rendered by the web canvas.
    @objc public func loadStrokes(_ invoke: Invoke) { invoke.resolve() }
    @objc public func addStrokes(_ invoke: Invoke) { invoke.resolve() }
    @objc public func removeStrokes(_ invoke: Invoke) { invoke.resolve() }

    @objc public func getMetrics(_ invoke: Invoke) {
        DispatchQueue.main.async { [self] in invoke.resolve(["counters": metrics, "pendingHandoffs": handoff.pending.count, "frame": NSCoder.string(for: preview?.frame ?? .zero)]) }
    }

    // MARK: geometry

    private func place() {
        guard let wv = webview, let host = wv.superview, let p = preview else { return }
        let sv = wv.scrollView
        let f = OverlayFrame.frame(cssX: cssRect.minX, cssY: cssRect.minY, cssW: cssRect.width, cssH: cssRect.height,
                                   zoom: sv.zoomScale, contentOffsetX: sv.contentOffset.x, contentOffsetY: sv.contentOffset.y)
        p.frame = wv.convert(CGRect(x: f.x, y: f.y, width: f.width, height: f.height), to: host)
        p.transform2 = transform
        p.setNeedsDisplay()
    }

    private func setShown(_ show: Bool) {
        visible = show
        capture?.isEnabled = show
        // the preview stays visible while hand-offs are pending even if capture stops
        preview?.isHidden = !show && handoff.pending.isEmpty
    }

    func gestureRecognizer(_ g: UIGestureRecognizer, shouldReceive touch: UITouch) -> Bool {
        guard visible, touch.type == .pencil, let p = preview else { return false }
        return p.bounds.contains(touch.location(in: p))
    }

    // MARK: input (called by PencilCapture)

    private func sample(_ t: UITouch) -> PencilSample {
        let loc = t.preciseLocation(in: preview)
        return PencilSample(x: Double(loc.x), y: Double(loc.y), force: Double(t.force), maximumForce: Double(t.maximumPossibleForce),
                            altitude: Double(t.altitudeAngle), azimuth: Double(t.azimuthAngle(in: preview)), timestamp: t.timestamp,
                            estimationIndex: t.estimationUpdateIndex?.intValue,
                            expectsForceUpdate: t.estimatedPropertiesExpectingUpdates.contains(.force))
    }

    func begin(_ t: UITouch, _ event: UIEvent) -> Bool {
        guard visible, let p = preview else { return false }
        let s = LiveStroke(brush: brush, transform: transform)
        s.add([sample(t)], coalesced: false)
        p.live = s
        p.setNeedsDisplay()
        metrics["strokes_begun", default: 0] += 1
        return true
    }

    func move(_ t: UITouch, _ event: UIEvent) {
        guard let p = preview, let s = p.live else { return }
        let list = event.coalescedTouches(for: t) ?? [t]
        s.add(list.map(sample), coalesced: true)
        s.setPredicted((event.predictedTouches(for: t) ?? []).map(sample))
        metrics["coalesced_samples", default: 0] += Double(list.count)
        p.setNeedsDisplay()
    }

    func end(_ t: UITouch, _ event: UIEvent, cancelled: Bool) {
        guard let p = preview, let s = p.live else { return }
        if !cancelled { s.add((event.coalescedTouches(for: t) ?? [t]).map(sample), coalesced: true) }
        p.live = nil
        if cancelled && s.points.count < 4 { p.setNeedsDisplay(); return }
        let json = s.finish()
        p.pending.append((s.id, s.points, s.brush))
        handoff.finished(s.id, at: CACurrentMediaTime())
        p.setNeedsDisplay()
        if let data = try? JSONSerialization.data(withJSONObject: json), let str = String(data: data, encoding: .utf8) {
            trigger("stroke", data: ["stroke": str])
        }
    }

    func estimated(_ t: UITouch) {
        guard let idx = t.estimationUpdateIndex?.intValue else { return }
        let final = !t.estimatedPropertiesExpectingUpdates.contains(.force)
        if preview?.live?.updateEstimated(index: idx, force: Double(t.force), maximumForce: Double(t.maximumPossibleForce), final: final) == true {
            metrics["estimated_force_updates", default: 0] += 1
        }
    }

    private func expire() {
        let gone = handoff.expire(now: CACurrentMediaTime())
        guard !gone.isEmpty, let p = preview else { return }
        p.pending.removeAll { gone.contains($0.id) }
        metrics["handoff_timeouts", default: 0] += Double(gone.count)
        if !visible && handoff.pending.isEmpty { p.isHidden = true }
        p.setNeedsDisplay()
        trigger("error", data: ["message": "native preview hand-off timed out for \(gone.count) stroke(s)"])
    }
}

@_cdecl("init_plugin_native_ink")
func initPlugin() -> Plugin {
    // As in M00: lift WKWebView's 60 fps rendering-update cap so the committed
    // web layer can also follow 120 Hz. Private SPI (App Store review risk);
    // applied only if WebKit still exposes it.
    WebFrameRate.installHighRefreshHook()
    return NativeInkPlugin()
}
#endif
