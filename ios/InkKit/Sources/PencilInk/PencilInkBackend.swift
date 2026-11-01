#if canImport(PencilKit) && canImport(UIKit)
import InkCore
import PencilKit
import UIKit

/// Reusable native PencilKit handwriting backend (InkBackend contract, see
/// docs/architecture.md). Shared by the standalone UIKit host (ios/M00iOS),
/// the Tauri iOS plugin and the Flutter iOS Platform View.
///
/// PencilKit renders ink itself with no host involvement. The backend only
/// observes *completed* drawing changes (canvasViewDrawingDidChange) and
/// reports them as canonical strokes, so no Pencil sample ever crosses into
/// JavaScript, Dart or Rust before it is visible.
///
/// Coordinates: canonical document units = PencilKit drawing coordinates
/// (points at zoom 1). Host viewport convention (same as every other
/// backend): viewPt = doc * scale + offset  ⇔  zoomScale = scale,
/// contentOffset = -offset.
public final class PencilInkBackend: NSObject, PKCanvasViewDelegate {
    public enum Tool: String { case pen, eraser }

    public let canvas = PKCanvasView()
    public let metrics = Metrics()

    /// Completed strokes appended by the user (canonical form, with backend ids).
    public var onStrokesCommitted: (([InkStroke]) -> Void)?
    /// Stroke ids removed (vector eraser or programmatic).
    public var onStrokesErased: (([String]) -> Void)?
    public var onViewportChanged: ((_ scale: Double, _ offsetX: Double, _ offsetY: Double) -> Void)?
    /// Lifecycle/diagnostic events: attached, detached, toolBegin, toolEnd, ...
    public var onEvent: ((String, [String: Any]) -> Void)?

    private(set) public var toolKind: Tool = .pen
    private(set) public var toolWidth: Double = 3
    private var ids: [String] = []
    private var keys: [String] = []
    private var suppress = 0
    private var toolEndTime: CFTimeInterval = 0
    private var toolBeginTime: CFTimeInterval = 0
    private var forceRange = (min: Double.infinity, max: -Double.infinity)
    private var attached = false
    private let forceProbe = ForceProbe(target: nil, action: nil)
    private var forceCalibration: [[String: JSONValue]] = []

    public static let documentSize = CGSize(width: 4000, height: 4000)

    public override init() {
        super.init()
        canvas.delegate = self
        canvas.backgroundColor = .white
        canvas.isOpaque = true
        canvas.drawingPolicy = .pencilOnly // fingers scroll/zoom, Pencil draws
        canvas.minimumZoomScale = 0.25
        canvas.maximumZoomScale = 8
        canvas.contentSize = Self.documentSize
        canvas.contentInsetAdjustmentBehavior = .never
        canvas.alwaysBounceVertical = false
        canvas.bouncesZoom = false
        canvas.overrideUserInterfaceStyle = .light // keep ink black (PencilKit inverts in dark mode)
        setTool(.pen, width: 3)
        canvas.addGestureRecognizer(forceProbe)
        metrics.info["backend"] = .string("pencilkit")
    }

    // MARK: InkBackend contract

    public func attachSurface(to parent: UIView, frame: CGRect) {
        canvas.frame = frame
        if canvas.superview !== parent { parent.addSubview(canvas) }
        attached = true
        metrics.event("attach_surface", ["w": .number(frame.width), "h": .number(frame.height)])
        onEvent?("attached", ["frame": NSCoder.string(for: frame)])
    }

    public func setFrame(_ frame: CGRect) {
        guard canvas.frame != frame else { return }
        canvas.frame = frame
        metrics.event("surface_frame", ["w": .number(frame.width), "h": .number(frame.height)])
    }

    public func detachSurface() {
        canvas.removeFromSuperview()
        attached = false
        metrics.event("detach_surface")
        onEvent?("detached", [:])
    }

    public func dispose() {
        detachSurface()
        canvas.delegate = nil
        onStrokesCommitted = nil; onStrokesErased = nil; onViewportChanged = nil; onEvent = nil
    }

    public func setTool(_ kind: Tool, width: Double) {
        toolKind = kind; toolWidth = width
        switch kind {
        case .pen: canvas.tool = PKInkingTool(.pen, color: .black, width: CGFloat(width))
        case .eraser: canvas.tool = PKEraserTool(.vector) // whole-stroke erase, same semantics as other backends
        }
    }

    public func setFingerDrawing(_ allowed: Bool) {
        canvas.drawingPolicy = allowed ? .anyInput : .pencilOnly
    }

    public func setViewport(scale: Double, offsetX: Double, offsetY: Double) {
        suppress += 1
        canvas.setZoomScale(CGFloat(scale), animated: false)
        canvas.setContentOffset(CGPoint(x: -offsetX, y: -offsetY), animated: false)
        suppress -= 1
    }

    public func viewport() -> (scale: Double, offsetX: Double, offsetY: Double) {
        (Double(canvas.zoomScale), Double(-canvas.contentOffset.x), Double(-canvas.contentOffset.y))
    }

    public func clear() {
        let removed = ids
        replaceDrawing(PKDrawing(), ids: [])
        metrics.event("clear_canvas")
        if !removed.isEmpty { onStrokesErased?(removed) }
    }

    /// Imports canonical strokes (replacing the drawing). Returns ms.
    @discardableResult
    public func loadStrokes(_ strokes: [InkStroke]) -> Double {
        let t0 = CACurrentMediaTime()
        let pk = strokes.map(PencilConvert.toPencil)
        replaceDrawing(PKDrawing(strokes: pk), ids: strokes.map(\.id))
        brushes = Dictionary(strokes.map { ($0.id, (brush: $0.brush, source: $0.source, input: $0.input)) }, uniquingKeysWith: { a, _ in a })
        let ms = (CACurrentMediaTime() - t0) * 1000
        metrics.timing("import_ms", ms)
        metrics.event("strokes_loaded", ["count": .number(Double(strokes.count))])
        return ms
    }

    /// Canonical brush and source metadata per stroke id for strokes created or
    /// loaded in this session (PencilKit strokes carry neither).
    private var brushes: [String: (brush: InkBrush, source: String, input: [String: JSONValue])] = [:]

    /// Canonical export without the session brush registry: what a host gets
    /// from the native PKDrawing alone (used by the interop test).
    public func exportStrokesFromNativeOnly() -> [InkStroke] {
        zip(canvas.drawing.strokes, ids).map { PencilConvert.toCanonical($0, id: $1, nominalWidth: nil) }
    }

    public func exportStrokes() -> [InkStroke] {
        let t0 = CACurrentMediaTime()
        // Brushes known in this session (live tool width, or the exact brush of
        // a loaded canonical stroke) win; only strokes with no record (e.g. a
        // restored PKDrawing) fall back to inverting the width model.
        let out = zip(canvas.drawing.strokes, ids).map { pk, id -> InkStroke in
            let c = PencilConvert.toCanonical(pk, id: id, nominalWidth: nil)
            guard let m = brushes[id] else { return c }
            // Session metadata (origin api/device, force calibration) wins; only the
            // fresh PencilKit diagnostics (pkSize*) are refreshed.
            return InkStroke(id: c.id, source: m.source, input: m.input.merging(c.input.filter { $0.key.hasPrefix("pk") }) { _, new in new }, brush: m.brush, points: c.points)
        }
        metrics.timing("export_ms", (CACurrentMediaTime() - t0) * 1000)
        return out
    }

    /// Native PKDrawing serialization (fidelity reference, not the document format).
    public func nativeDrawingData() -> Data { canvas.drawing.dataRepresentation() }

    public func loadNativeDrawing(_ data: Data) throws {
        let d = try PKDrawing(data: data)
        replaceDrawing(d, ids: d.strokes.map { _ in UUID().uuidString.lowercased() })
    }

    public func strokeCount() -> Int { canvas.drawing.strokes.count }

    public func getCapabilities() -> [String: Any] {
        [
            "backend": "pencilkit",
            "drawingPolicy": canvas.drawingPolicy == .pencilOnly ? "pencilOnly" : "anyInput",
            "eraser": "vector",
            "zoom": ["min": canvas.minimumZoomScale, "max": canvas.maximumZoomScale],
            "documentSize": ["w": Self.documentSize.width, "h": Self.documentSize.height],
            "os": UIDevice.current.systemVersion,
            "attached": attached,
            "pencilMaxForceAssumed": NativeMapping.pencilMaxForce,
            "observedForceRange": forceRange.min.isFinite ? [forceRange.min, forceRange.max] : [],
            "forceCalibration": forceCalibration.map { $0.mapValues { v -> Double in if case let .number(n) = v { return n }; return 0 } },
        ]
    }

    public func getMetrics() -> [String: Any] {
        var m = metrics.toJSON()
        m["capabilities"] = getCapabilities()
        m["strokeCount"] = strokeCount()
        return m
    }

    // MARK: PKCanvasViewDelegate

    public func canvasViewDidBeginUsingTool(_ canvasView: PKCanvasView) {
        toolBeginTime = CACurrentMediaTime()
        metrics.count("tool_begin")
        onEvent?("toolBegin", [:])
    }

    public func canvasViewDidEndUsingTool(_ canvasView: PKCanvasView) {
        toolEndTime = CACurrentMediaTime()
        metrics.timing("stroke_duration_ms", (toolEndTime - toolBeginTime) * 1000)
        metrics.count("tool_end")
        onEvent?("toolEnd", [:])
    }

    public func canvasViewDidFinishRendering(_ canvasView: PKCanvasView) {
        metrics.count("finish_rendering")
    }

    public func canvasViewDrawingDidChange(_ canvasView: PKCanvasView) {
        metrics.count("drawing_did_change")
        guard suppress == 0 else { return }
        let t0 = CACurrentMediaTime()
        let strokes = canvasView.drawing.strokes
        let newKeys = strokes.map(PencilConvert.key)
        var oldByKey: [String: String] = [:]
        for (k, id) in zip(keys, ids) { oldByKey[k] = id }
        var newIds: [String] = []
        var added: [InkStroke] = []
        for (i, s) in strokes.enumerated() {
            if let id = oldByKey.removeValue(forKey: newKeys[i]) {
                newIds.append(id)
            } else {
                var c = PencilConvert.toCanonical(s, id: UUID().uuidString.lowercased(), nominalWidth: toolKind == .pen ? toolWidth : nil)
                var pkForces: [Double] = []
                for p in s.path {
                    forceRange.min = min(forceRange.min, Double(p.force)); forceRange.max = max(forceRange.max, Double(p.force))
                    pkForces.append(Double(p.force))
                }
                // Calibration: raw UITouch.force of the same stroke vs what PencilKit stored.
                let probe = forceProbe.take()
                if !probe.forces.isEmpty {
                    func q(_ v: [Double], _ f: Double) -> Double { let s = v.sorted(); return s[min(s.count - 1, Int(Double(s.count) * f))] }
                    let cal: [String: JSONValue] = [
                        "touchForceMedian": .number(q(probe.forces, 0.5)), "touchForceP90": .number(q(probe.forces, 0.9)),
                        "touchForceMax": .number(probe.forces.max()!), "touchSamples": .number(Double(probe.forces.count)),
                        "touchMaxPossibleForce": .number(probe.maxPossible),
                        "pkForceMedian": .number(q(pkForces, 0.5)), "pkForceP90": .number(q(pkForces, 0.9)),
                        "pkForceMax": .number(pkForces.max() ?? 0), "pkPoints": .number(Double(pkForces.count)),
                    ]
                    var input = c.input
                    for (k, v) in cal { input[k] = v }
                    c = InkStroke(id: c.id, source: c.source, input: input, brush: c.brush, points: c.points)
                    forceCalibration.append(cal)
                }
                newIds.append(c.id)
                if toolKind == .pen { brushes[c.id] = (brush: c.brush, source: c.source, input: c.input) }
                added.append(c)
            }
        }
        let removed = Array(oldByKey.values)
        for id in removed { brushes.removeValue(forKey: id) }
        ids = newIds; keys = newKeys
        if toolEndTime > 0 { metrics.timing("tool_end_to_commit_ms", (t0 - toolEndTime) * 1000) }
        metrics.timing("diff_convert_ms", (CACurrentMediaTime() - t0) * 1000)
        if !added.isEmpty {
            metrics.count("strokes_committed", added.count)
            for s in added {
                metrics.stroke(["id": .string(s.id), "samples": .number(Double(s.points.count)),
                                "durationMs": .number(Double((s.points.last?.timeUs ?? 0) - (s.points.first?.timeUs ?? 0)) / 1000)])
            }
            onStrokesCommitted?(added)
        }
        if !removed.isEmpty {
            metrics.count("strokes_erased", removed.count)
            onStrokesErased?(removed)
        }
    }

    public func scrollViewDidScroll(_ scrollView: UIScrollView) { viewportChanged() }
    public func scrollViewDidZoom(_ scrollView: UIScrollView) { viewportChanged() }

    private func viewportChanged() {
        guard suppress == 0 else { return }
        let v = viewport()
        onViewportChanged?(v.scale, v.offsetX, v.offsetY)
    }

    private func replaceDrawing(_ d: PKDrawing, ids newIds: [String]) {
        suppress += 1
        canvas.drawing = d
        brushes = [:]
        ids = newIds
        keys = d.strokes.map(PencilConvert.key)
        suppress -= 1
    }
}

/// PencilKit <-> canonical conversion. See docs/interop.md for what is lossy.
public enum PencilConvert {
    /// Identity key for diffing PKDrawing snapshots (PKStroke has no id).
    static func key(_ s: PKStroke) -> String {
        let p = s.path
        let first = p.first?.location ?? .zero
        let last = p.last?.location ?? .zero
        return "\(p.creationDate.timeIntervalSinceReferenceDate)|\(p.count)|\(first.x),\(first.y)|\(last.x),\(last.y)|\(s.transform.tx),\(s.transform.ty)"
    }

    /// Native stroke -> canonical. Positions are the path's control points in
    /// drawing coordinates (stroke.transform applied). Pressure = force / Pencil
    /// max force. Per-point PencilKit sizes are summarised in input.pkSize*
    /// (the canonical format has a single nominal width per stroke).
    public static func toCanonical(_ s: PKStroke, id: String, nominalWidth: Double?) -> InkStroke {
        let t = s.transform
        let path = s.path
        var pts: [InkPoint] = []
        pts.reserveCapacity(path.count)
        var sizes: [Double] = []
        for p in path {
            let loc = p.location.applying(t)
            let tilt = NativeMapping.tilt(altitude: Double(p.altitude), azimuth: Double(p.azimuth))
            pts.append(InkPoint(x: Double(loc.x), y: Double(loc.y),
                                pressure: NativeMapping.pressure(fromForce: Double(p.force)),
                                tiltX: tilt.x, tiltY: tilt.y,
                                timeUs: Int64((p.timeOffset * 1_000_000).rounded())))
            sizes.append(Double(p.size.width))
        }
        let sorted = sizes.sorted()
        let median = sorted.isEmpty ? 3 : sorted[sorted.count / 2]
        // Nominal width: the tool width when known (live strokes); otherwise
        // invert the shared width model (strokes imported from canonical form).
        let (width, pressureOn) = nominalWidth.map { ($0, true) } ?? NativeMapping.invertWidth(sizes: sizes, pressures: pts.map(\.pressure))
        return InkStroke(
            id: id, source: "stylus",
            input: [
                "api": .string("pencilkit"), "device": .string("pencil"),
                "ink": .string(s.ink.inkType.rawValue),
                "pkSizeMin": .number(sorted.first ?? 0), "pkSizeMedian": .number(median), "pkSizeMax": .number(sorted.last ?? 0),
                "pkCreated": .number(path.creationDate.timeIntervalSince1970),
            ],
            brush: InkBrush(width: width, pressure: pressureOn),
            points: pts)
    }

    /// Canonical -> native stroke. Size per point from the shared width model;
    /// altitude/azimuth from tilt (zero tilt -> perpendicular pen).
    public static func toPencil(_ s: InkStroke) -> PKStroke {
        var cps: [PKStrokePoint] = []
        cps.reserveCapacity(max(1, s.points.count))
        for p in s.points {
            let w = CGFloat(NativeMapping.importWidth(s, p))
            let aa = NativeMapping.altitudeAzimuth(tiltX: p.tiltX, tiltY: p.tiltY)
            cps.append(PKStrokePoint(location: CGPoint(x: p.x, y: p.y),
                                     timeOffset: TimeInterval(p.timeUs) / 1_000_000,
                                     size: CGSize(width: w, height: w), opacity: 1,
                                     force: CGFloat(NativeMapping.force(fromPressure: p.pressure)),
                                     azimuth: CGFloat(aa.azimuth), altitude: CGFloat(aa.altitude)))
        }
        let path = PKStrokePath(controlPoints: cps, creationDate: Date())
        return PKStroke(ink: PKInk(.pen, color: .black), path: path)
    }
}

/// Rasterisation-based fidelity metric: ink-coverage IoU between two images.
public enum Fidelity {
    /// Renders canonical strokes with the shared geometry (CoreGraphics).
    public static func renderCanonical(_ strokes: [InkStroke], rect: CGRect, scale: CGFloat = 1) -> UIImage {
        let fmt = UIGraphicsImageRendererFormat()
        fmt.scale = scale; fmt.opaque = true
        return UIGraphicsImageRenderer(bounds: rect, format: fmt).image { ctx in
            let c = ctx.cgContext
            c.setFillColor(UIColor.white.cgColor); c.fill(rect)
            c.setStrokeColor(UIColor.black.cgColor); c.setFillColor(UIColor.black.cgColor)
            c.setLineCap(.round); c.setLineJoin(.round)
            for s in strokes {
                for pc in Geometry.pieces(s.points, s.brush.width, s.brush.pressure) {
                    switch pc {
                    case let .dot(x, y, w): c.fillEllipse(in: CGRect(x: x - w / 2, y: y - w / 2, width: w, height: w))
                    case let .line(x0, y0, x1, y1, w):
                        c.setLineWidth(w); c.move(to: CGPoint(x: x0, y: y0)); c.addLine(to: CGPoint(x: x1, y: y1)); c.strokePath()
                    case let .quad(x0, y0, cx, cy, x1, y1, w):
                        c.setLineWidth(w); c.move(to: CGPoint(x: x0, y: y0))
                        c.addQuadCurve(to: CGPoint(x: x1, y: y1), control: CGPoint(x: cx, y: cy)); c.strokePath()
                    }
                }
            }
        }
    }

    /// PencilKit rendering of a drawing over `rect` on white.
    public static func renderPencil(_ d: PKDrawing, rect: CGRect, scale: CGFloat = 1) -> UIImage {
        let fmt = UIGraphicsImageRendererFormat()
        fmt.scale = scale; fmt.opaque = true
        let ink = UITraitCollection(userInterfaceStyle: .light)
        var img: UIImage!
        ink.performAsCurrent { img = d.image(from: rect, scale: scale) }
        return UIGraphicsImageRenderer(bounds: rect, format: fmt).image { ctx in
            UIColor.white.setFill(); ctx.fill(rect)
            img.draw(in: rect)
        }
    }

    /// Coverage = pixels darker than 50%. Returns IoU, coverage of each, and a
    /// tolerance IoU where B is dilated by `tolerancePx` (forgives sub-pixel offsets).
    public static func compare(_ a: UIImage, _ b: UIImage, tolerancePx: Int = 1) -> [String: Double] {
        guard let ma = mask(a), let mb = mask(b), ma.w == mb.w, ma.h == mb.h else { return ["error": 1] }
        var inter = 0, union = 0, ca = 0, cb = 0
        for i in 0..<ma.bits.count {
            let x = ma.bits[i], y = mb.bits[i]
            if x { ca += 1 }; if y { cb += 1 }
            if x && y { inter += 1 }; if x || y { union += 1 }
        }
        let db = dilate(mb, tolerancePx), da = dilate(ma, tolerancePx)
        var aInB = 0, bInA = 0
        for i in 0..<ma.bits.count {
            if ma.bits[i] && db[i] { aInB += 1 }
            if mb.bits[i] && da[i] { bInA += 1 }
        }
        return ["iou": union > 0 ? Double(inter) / Double(union) : 1,
                "coverageA": Double(ca), "coverageB": Double(cb),
                "aCoveredByB_tol": ca > 0 ? Double(aInB) / Double(ca) : 1,
                "bCoveredByA_tol": cb > 0 ? Double(bInA) / Double(cb) : 1,
                "inkRatioBoverA": ca > 0 ? Double(cb) / Double(ca) : 0]
    }

    struct Mask { let w: Int, h: Int, bits: [Bool] }

    static func mask(_ img: UIImage) -> Mask? {
        guard let cg = img.cgImage else { return nil }
        let w = cg.width, h = cg.height
        var buf = [UInt8](repeating: 255, count: w * h)
        guard let ctx = CGContext(data: &buf, width: w, height: h, bitsPerComponent: 8, bytesPerRow: w,
                                  space: CGColorSpaceCreateDeviceGray(), bitmapInfo: CGImageAlphaInfo.none.rawValue) else { return nil }
        ctx.draw(cg, in: CGRect(x: 0, y: 0, width: w, height: h))
        return Mask(w: w, h: h, bits: buf.map { $0 < 128 })
    }

    static func dilate(_ m: Mask, _ r: Int) -> [Bool] {
        guard r > 0 else { return m.bits }
        var out = m.bits
        for y in 0..<m.h { for x in 0..<m.w where m.bits[y * m.w + x] {
            for dy in -r...r { for dx in -r...r {
                let xx = x + dx, yy = y + dy
                if xx >= 0, yy >= 0, xx < m.w, yy < m.h { out[yy * m.w + xx] = true }
            } }
        } }
        return out
    }
}
#endif
