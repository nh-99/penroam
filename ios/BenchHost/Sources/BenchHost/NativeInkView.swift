#if canImport(UIKit)
import InkCore
import QuartzCore
import UIKit

/// Native UIKit Apple Pencil reference (PLAN §9, diagnostic). Same geometry,
/// layering and metric names as the Flutter/Tauri prototypes:
///   raw samples    -> active.points (+ UITouch timestamps)
///   active preview -> stable pieces are drawn into the committed bitmap once
///                     per frame (flushStable); ActiveLayerView draws only the
///                     provisional tail + predicted points (full redraw, tiny)
///   committed      -> strokes
///   cache          -> committedCtx bitmap shown via committedLayer.contents
/// Implicit CALayer animations are disabled: a contents change must never
/// cross-fade (that caused ink to vanish briefly on pen-up).
/// Uses coalesced touches for completeness; predicted touches only as a
/// temporary preview (never committed); estimated-force updates patch
/// pressure in place.
final class NativeInkView: UIView, UIPencilInteractionDelegate {
    enum Tool { case pen, eraser }

    let metrics = Metrics()
    private(set) var strokes: [InkStroke] = []
    var width = 3.0
    var pressureEnabled = true
    var tool: Tool = .pen
    var polyline = false { didSet { rebuild() } }
    var usePredicted = false
    var onChange: (() -> Void)?

    // view transform: viewPt = doc * scale + offset (points)
    private(set) var scale: CGFloat = 1
    private(set) var offset = CGPoint.zero

    private let committedLayer = CALayer()
    private var committedCtx: CGContext?
    private let activeView = ActiveLayerView()

    private final class Active {
        let stroke: InkStroke
        let touch: UITouch
        let eraser: Bool
        let t0: TimeInterval
        var ts: [Double] = []
        var coalesced = 0
        var predicted: [InkPoint] = []
        var predictedShown = 0
        var stableDrawn = 0
        var erased: [String] = []
        init(_ s: InkStroke, _ t: UITouch, _ e: Bool, _ t0: TimeInterval) { stroke = s; touch = t; eraser = e; self.t0 = t0 }
    }
    private var active: Active?
    private var estimation: [NSNumber: (InkStroke, Int)] = [:]
    private var pendingTouchTs: TimeInterval = 0
    private var displayLink: CADisplayLink?
    private var lastFrameTs: CFTimeInterval = 0
    private var navTouches: [UITouch: CGPoint] = [:]
    private var pinch: (dist: CGFloat, scale: CGFloat, anchor: CGPoint)?
    private var commitStart: CFTimeInterval = -1

    override init(frame: CGRect) {
        super.init(frame: frame)
        backgroundColor = .white
        isMultipleTouchEnabled = true
        layer.addSublayer(committedLayer)
        committedLayer.contentsGravity = .topLeft
        committedLayer.actions = ["contents": NSNull(), "bounds": NSNull(), "position": NSNull(), "frame": NSNull()]
        activeView.owner = self
        activeView.isUserInteractionEnabled = false
        activeView.backgroundColor = .clear
        activeView.isOpaque = false
        activeView.clearsContextBeforeDrawing = true
        addSubview(activeView)
        let pi = UIPencilInteraction()
        pi.delegate = self
        addInteraction(pi)
        let hover = UIHoverGestureRecognizer(target: self, action: #selector(onHover(_:)))
        addGestureRecognizer(hover)
        metrics.info["pencilPreferredTapAction"] = .string("\(UIPencilInteraction.preferredTapAction.rawValue)")
    }

    required init?(coder: NSCoder) { fatalError() }

    override func didMoveToWindow() {
        super.didMoveToWindow()
        if window != nil, displayLink == nil {
            let dl = CADisplayLink(target: self, selector: #selector(onFrame(_:)))
            dl.preferredFrameRateRange = CAFrameRateRange(minimum: 60, maximum: 120, preferred: 120)
            dl.add(to: .main, forMode: .common)
            displayLink = dl
        } else if window == nil {
            displayLink?.invalidate(); displayLink = nil
        }
    }

    override func layoutSubviews() {
        super.layoutSubviews()
        activeView.frame = bounds
        if committedLayer.frame != bounds || committedCtx == nil {
            committedLayer.frame = bounds; rebuild(reason: committedCtx == nil ? "first-layout" : "resize")
        }
    }

    // MARK: public

    func setStrokes(_ s: [InkStroke]) -> Double { strokes = s; return rebuild(reason: "load") }

    func clear() {
        strokes = []; active = nil
        metrics.event("clear_canvas")
        rebuild(reason: "clear"); activeView.setNeedsDisplay(); onChange?()
    }

    func setTransform(scale s: CGFloat, offset o: CGPoint) {
        scale = min(8, max(0.25, s)); offset = o
        rebuild(reason: "transform"); activeView.setNeedsDisplay()
    }

    func toDoc(_ p: CGPoint) -> CGPoint { CGPoint(x: (p.x - offset.x) / scale, y: (p.y - offset.y) / scale) }

    /// Full rebuild of the committed bitmap. Returns ms.
    @discardableResult
    func rebuild(reason: String = "explicit") -> Double {
        let t0 = CACurrentMediaTime()
        // Timestamped so a visible flash can be matched to a rebuild (or ruled out).
        metrics.event("full_rebuild", ["reason": .string(reason), "strokes": .number(Double(strokes.count)),
                                       "activeStroke": .bool(active != nil)])
        let s = window?.screen.scale ?? UIScreen.main.scale
        let w = Int(bounds.width * s), h = Int(bounds.height * s)
        guard w > 0, h > 0 else { return 0 }
        if committedCtx == nil || committedCtx!.width != w || committedCtx!.height != h {
            committedCtx = CGContext(data: nil, width: w, height: h, bitsPerComponent: 8, bytesPerRow: 0,
                                     space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)
            committedCtx?.translateBy(x: 0, y: CGFloat(h)); committedCtx?.scaleBy(x: s, y: -s) // UIKit-style coords
            committedLayer.contentsScale = s
        }
        guard let c = committedCtx else { return 0 }
        c.saveGState()
        c.clear(CGRect(x: 0, y: 0, width: bounds.width, height: bounds.height))
        applyDocTransform(c)
        let tl = toDoc(.zero), br = toDoc(CGPoint(x: bounds.width, y: bounds.height))
        for st in strokes {
            let b = st.bounds
            if b[2] < tl.x || b[0] > br.x || b[3] < tl.y || b[1] > br.y { continue }
            InkRender.draw(c, st, 0, Geometry.pieceCount(st.points.count), polyline: polyline)
        }
        c.restoreGState()
        committedLayer.contents = c.makeImage()
        active?.stableDrawn = 0 // bitmap was cleared: the preview must redraw the whole active stroke
        activeView.setNeedsDisplay()
        let ms = (CACurrentMediaTime() - t0) * 1000
        metrics.timing("full_rebuild_ms", ms)
        metrics.count("full_rebuilds")
        return ms
    }

    func applyDocTransform(_ c: CGContext) {
        c.translateBy(x: offset.x, y: offset.y)
        c.scaleBy(x: scale, y: scale)
    }

    // MARK: touches

    override func touchesBegan(_ touches: Set<UITouch>, with event: UIEvent?) {
        for t in touches {
            let h = CACurrentMediaTime()
            metrics.count("pointer_event_received")
            metrics.timing("event_to_handler_ms", (h - t.timestamp) * 1000)
            if t.type == .pencil || (t.type == .indirectPointer) {
                recordDevice(t)
                guard active == nil else { continue }
                startStroke(t, event, handlerStart: h)
            } else {
                if active != nil { metrics.count("palm_rejected_while_stylus"); continue }
                if t.majorRadius > 40 { metrics.count("palm_rejected_large_contact"); continue }
                navTouches[t] = t.location(in: self)
                if navTouches.count == 2 { beginPinch() }
            }
        }
    }

    override func touchesMoved(_ touches: Set<UITouch>, with event: UIEvent?) {
        for t in touches {
            let h = CACurrentMediaTime()
            if navTouches[t] != nil { navigate(t); continue }
            guard let a = active, a.touch === t else { continue }
            metrics.count("pointer_event_received")
            metrics.timing("event_to_handler_ms", (h - t.timestamp) * 1000)
            let list = event?.coalescedTouches(for: t) ?? [t]
            a.coalesced += list.count
            for ct in list { addSample(ct) }
            if usePredicted { a.predicted = (event?.predictedTouches(for: t) ?? []).map { point($0, a) } }
            afterInput(h, t.timestamp)
        }
    }

    override func touchesEnded(_ touches: Set<UITouch>, with event: UIEvent?) { end(touches, event, cancelled: false) }
    override func touchesCancelled(_ touches: Set<UITouch>, with event: UIEvent?) { end(touches, event, cancelled: true) }

    override func touchesEstimatedPropertiesUpdated(_ touches: Set<UITouch>) {
        for t in touches {
            guard let idx = t.estimationUpdateIndex, let (s, i) = estimation[idx], i < s.points.count else { continue }
            if t.maximumPossibleForce > 0 { s.points[i].pressure = Double(t.force / t.maximumPossibleForce) }
            metrics.count("estimated_force_updates")
            if !t.estimatedPropertiesExpectingUpdates.contains(.force) { estimation[idx] = nil }
        }
    }

    private func end(_ touches: Set<UITouch>, _ event: UIEvent?, cancelled: Bool) {
        for t in touches {
            if navTouches.removeValue(forKey: t) != nil { if navTouches.count < 2 { pinch = nil }; continue }
            guard let a = active, a.touch === t else { continue }
            if cancelled { metrics.count("stroke_cancelled") }
            else {
                for ct in event?.coalescedTouches(for: t) ?? [t] where ct.timestamp > (a.ts.last ?? 0) / 1000 + a.t0 - 1e-6 {
                    addSample(ct)
                }
            }
            finish()
        }
    }

    private func recordDevice(_ t: UITouch) {
        guard metrics.info["device_pencil"] == nil else { return }
        metrics.info["device_pencil"] = .string("maxForce=\(t.maximumPossibleForce) force=\(t.force) altitude=\(t.altitudeAngle) azimuth=\(t.azimuthAngle(in: self)) estimatedProps=\(t.estimatedProperties.rawValue) expectingUpdates=\(t.estimatedPropertiesExpectingUpdates.rawValue) rollAngle=\(t.rollAngle)")
    }

    private func startStroke(_ t: UITouch, _ event: UIEvent?, handlerStart: CFTimeInterval) {
        let eraser = tool == .eraser
        let s = InkStroke(source: t.type == .pencil ? "stylus" : "mouse",
                          input: ["api": .string("uikit-touch"), "device": .string(t.type == .pencil ? "pencil" : "pointer")],
                          brush: InkBrush(width: width, pressure: pressureEnabled && t.type == .pencil), points: [])
        let a = Active(s, t, eraser, t.timestamp)
        active = a
        addSample(t)
        afterInput(handlerStart, t.timestamp)
    }

    private func point(_ t: UITouch, _ a: Active) -> InkPoint {
        let d = toDoc(t.preciseLocation(in: self))
        let p = t.type == .pencil && t.maximumPossibleForce > 0 ? Double(t.force / t.maximumPossibleForce) : 0.5
        // W3C tiltX/tiltY (degrees) from altitude/azimuth.
        let alt = Double(t.altitudeAngle), az = Double(t.azimuthAngle(in: self))
        let tanAlt = tan(alt)
        let tx = tanAlt == 0 ? 0 : atan(cos(az) / tanAlt) * 180 / .pi
        let ty = tanAlt == 0 ? 0 : atan(sin(az) / tanAlt) * 180 / .pi
        return InkPoint(x: Double(d.x), y: Double(d.y), pressure: min(1, max(0, p)), tiltX: tx, tiltY: ty,
                        timeUs: Int64(((t.timestamp - a.t0) * 1_000_000).rounded()))
    }

    private func addSample(_ t: UITouch) {
        guard let a = active else { return }
        let p = point(t, a)
        a.stroke.points.append(p)
        if a.eraser { eraseAt(p); return }
        a.ts.append((t.timestamp - a.t0) * 1000)
        if let idx = t.estimationUpdateIndex, t.estimatedPropertiesExpectingUpdates.contains(.force) {
            estimation[idx] = (a.stroke, a.stroke.points.count - 1)
        }
        metrics.count("input_samples")
    }

    private func afterInput(_ handlerStart: CFTimeInterval, _ touchTs: TimeInterval) {
        metrics.timing("input_sample_processed_ms", (CACurrentMediaTime() - handlerStart) * 1000)
        if pendingTouchTs == 0 { pendingTouchTs = touchTs }
        metrics.count("preview_update_scheduled")
        guard let a = active, !a.eraser else { return }
        needsFlush = true
        activeView.setNeedsDisplay()
    }

    private var needsFlush = false

    /// Draws newly stable pieces of the active stroke into the persistent
    /// bitmap and publishes it. Called once per display frame (and on pen-up).
    private func flushStable() {
        guard let a = active, !a.eraser, let c = committedCtx else { return }
        let stable = Geometry.stablePieceCount(a.stroke.points.count)
        guard stable > a.stableDrawn else { return }
        let t0 = CACurrentMediaTime()
        c.saveGState(); applyDocTransform(c)
        InkRender.draw(c, a.stroke, a.stableDrawn, stable, polyline: polyline)
        c.restoreGState()
        a.stableDrawn = stable
        CATransaction.begin(); CATransaction.setDisableActions(true)
        committedLayer.contents = c.makeImage()
        CATransaction.commit()
        metrics.timing("stable_flush_ms", (CACurrentMediaTime() - t0) * 1000)
    }

    func docToView(_ r: CGRect) -> CGRect {
        CGRect(x: r.minX * scale + offset.x, y: r.minY * scale + offset.y, width: r.width * scale, height: r.height * scale)
    }

    fileprivate func drawActive(_ c: CGContext, _ rect: CGRect) {
        let t0 = CACurrentMediaTime()
        guard let a = active, !a.eraser else { return }
        c.saveGState()
        applyDocTransform(c)
        let pts = a.stroke.points
        // Everything not yet flushed into the bitmap (normally just the tail).
        InkRender.draw(c, a.stroke, a.stableDrawn, Geometry.pieceCount(pts.count), polyline: polyline)
        if !a.predicted.isEmpty, let last = pts.last {
            let tmp = InkStroke(source: "predicted", input: [:], brush: a.stroke.brush, points: [last] + a.predicted)
            c.setAlpha(0.5)
            InkRender.draw(c, tmp, 0, Geometry.pieceCount(tmp.points.count), polyline: polyline)
            c.setAlpha(1)
            a.predictedShown += a.predicted.count
            var r = CGRect.null
            for p in tmp.points { r = r.union(CGRect(x: p.x - 10, y: p.y - 10, width: 20, height: 20)) }
            activeView.lastPredictedRect = r
        } else { activeView.lastPredictedRect = .null }
        c.restoreGState()
        metrics.timing("preview_render_submitted_ms", (CACurrentMediaTime() - t0) * 1000)
    }

    @objc private func onFrame(_ dl: CADisplayLink) {
        if lastFrameTs > 0 {
            let dt = (dl.timestamp - lastFrameTs) * 1000
            if dt < 250 {
                metrics.timing("frame_interval_ms", dt)
                if dt > 1000.0 / Double(max(60, dl.preferredFrameRateRange.preferred ?? 120)) * 1.5 { metrics.count("frames_over_budget") }
            }
        }
        lastFrameTs = dl.timestamp
        if needsFlush { needsFlush = false; flushStable() }
        if pendingTouchTs > 0 {
            // targetTimestamp = when the frame being prepared is expected on screen.
            metrics.timing("event_to_expected_present_ms", (dl.targetTimestamp - pendingTouchTs) * 1000)
            pendingTouchTs = 0
        }
        if commitStart >= 0 {
            metrics.timing("persistent_render_completed_ms", (CACurrentMediaTime() - commitStart) * 1000)
            commitStart = -1
        }
    }

    private func finish() {
        guard let a = active else { return }
        active = nil
        if a.eraser {
            metrics.event("erase_finished", ["removed": .number(Double(a.erased.count))])
            onChange?(); return
        }
        commitStart = CACurrentMediaTime()
        strokes.append(a.stroke)
        metrics.event("stroke_finalized", ["points": .number(Double(a.stroke.points.count))])
        // Same transaction: bitmap gains the remaining pieces and the preview
        // tail is cleared, so the stroke can neither vanish nor double up.
        CATransaction.begin(); CATransaction.setDisableActions(true)
        if let c = committedCtx {
            c.saveGState(); applyDocTransform(c)
            InkRender.draw(c, a.stroke, a.stableDrawn, Geometry.pieceCount(a.stroke.points.count), polyline: polyline)
            c.restoreGState()
            committedLayer.contents = c.makeImage()
        }
        activeView.setNeedsDisplay()
        CATransaction.commit()
        needsFlush = false
        metrics.timing("commit_cpu_ms", (CACurrentMediaTime() - commitStart) * 1000)
        let pts = a.stroke.points
        var st: [String: JSONValue] = [
            "id": .string(a.stroke.id), "device": .string("pencil"), "samples": .number(Double(pts.count)),
            "coalescedSamples": .number(Double(a.coalesced)), "predictedSamplesShown": .number(Double(a.predictedShown)),
            "durationMs": .number((a.ts.last ?? 0) - (a.ts.first ?? 0)),
        ]
        for (k, v) in analyzeSamples(a.ts, pts.map(\.x), pts.map(\.y)) { st[k] = .number(v) }
        metrics.stroke(st)
        onChange?()
    }

    private func eraseAt(_ p: InkPoint) {
        let r = 8 / Double(scale)
        let before = strokes.count
        strokes.removeAll { s in
            let hit = Geometry.hitTest(s, p.x, p.y, r)
            if hit { active?.erased.append(s.id) }
            return hit
        }
        if strokes.count != before { metrics.count("strokes_erased", before - strokes.count); rebuild() }
    }

    // MARK: navigation (fingers)

    private func beginPinch() {
        let v = Array(navTouches.values)
        let f = CGPoint(x: (v[0].x + v[1].x) / 2, y: (v[0].y + v[1].y) / 2)
        pinch = (max(1, hypot(v[1].x - v[0].x, v[1].y - v[0].y)), scale, toDoc(f))
    }

    private func navigate(_ t: UITouch) {
        let prev = navTouches[t]!
        let p = t.location(in: self)
        navTouches[t] = p
        if navTouches.count >= 2, let pz = pinch {
            let v = Array(navTouches.values)
            let f = CGPoint(x: (v[0].x + v[1].x) / 2, y: (v[0].y + v[1].y) / 2)
            let s = min(8, max(0.25, pz.scale * max(1, hypot(v[1].x - v[0].x, v[1].y - v[0].y)) / pz.dist))
            setTransform(scale: s, offset: CGPoint(x: f.x - pz.anchor.x * s, y: f.y - pz.anchor.y * s))
        } else if navTouches.count == 1 {
            setTransform(scale: scale, offset: CGPoint(x: offset.x + p.x - prev.x, y: offset.y + p.y - prev.y))
        }
    }

    // MARK: Pencil capabilities

    @available(iOS 17.5, *)
    func pencilInteraction(_ interaction: UIPencilInteraction, didReceiveTap tap: UIPencilInteraction.Tap) {
        metrics.event("pencil_double_tap")
    }

    @available(iOS 17.5, *)
    func pencilInteraction(_ interaction: UIPencilInteraction, didReceiveSqueeze squeeze: UIPencilInteraction.Squeeze) {
        if squeeze.phase == .ended { metrics.event("pencil_squeeze") }
    }

    @objc private func onHover(_ g: UIHoverGestureRecognizer) {
        if g.state == .began { metrics.event("pencil_hover_began") }
        metrics.count("hover_events")
    }

    // MARK: scripted replay (Workload D)

    func replay(_ pts: [InkPoint], completion: @escaping ([Double]) -> Void) {
        let s = InkStroke(source: "fixture", input: ["api": .string("replay")], brush: InkBrush(width: 3), points: [])
        let fake = UITouch()
        let a = Active(s, fake, false, CACurrentMediaTime())
        active = a
        var frames: [Double] = []
        var i = 0
        func step() {
            guard i < pts.count else { finish(); completion(frames); return }
            let t0 = CACurrentMediaTime()
            s.points.append(pts[i]); a.ts.append(Double(i) * 8)
            i += 1
            flushStable()
            activeView.setNeedsDisplay()
            activeView.layer.displayIfNeeded()
            frames.append((CACurrentMediaTime() - t0) * 1000)
            DispatchQueue.main.asyncAfter(deadline: .now() + 1.0 / 120) { step() }
        }
        step()
    }
}

final class ActiveLayerView: UIView {
    weak var owner: NativeInkView?
    var lastPredictedRect = CGRect.null
    override func draw(_ rect: CGRect) {
        guard let c = UIGraphicsGetCurrentContext() else { return }
        owner?.drawActive(c, rect)
    }
}

enum InkRender {
    static func box(_ p: Piece) -> CGRect {
        switch p {
        case let .dot(x, y, w): return CGRect(x: x - w, y: y - w, width: 2 * w, height: 2 * w)
        case let .line(x0, y0, x1, y1, w):
            return CGRect(x: min(x0, x1) - w, y: min(y0, y1) - w, width: abs(x1 - x0) + 2 * w, height: abs(y1 - y0) + 2 * w)
        case let .quad(x0, y0, cx, cy, x1, y1, w):
            let xs = [x0, cx, x1], ys = [y0, cy, y1]
            return CGRect(x: xs.min()! - w, y: ys.min()! - w, width: xs.max()! - xs.min()! + 2 * w, height: ys.max()! - ys.min()! + 2 * w)
        }
    }

    static func draw(_ c: CGContext, _ s: InkStroke, _ from: Int, _ to: Int, polyline: Bool) {
        let pts = s.points, n = pts.count
        guard n > 0, from < to else { return }
        c.setStrokeColor(UIColor.black.cgColor); c.setFillColor(UIColor.black.cgColor)
        c.setLineCap(.round); c.setLineJoin(.round)
        if polyline {
            c.setLineWidth(s.brush.width)
            if n == 1 { c.fillEllipse(in: CGRect(x: pts[0].x - s.brush.width / 2, y: pts[0].y - s.brush.width / 2, width: s.brush.width, height: s.brush.width)); return }
            let start = max(0, from - 1), end = min(n - 1, to)
            c.move(to: CGPoint(x: pts[start].x, y: pts[start].y))
            for i in (start + 1)...max(start + 1, end) where i < n { c.addLine(to: CGPoint(x: pts[i].x, y: pts[i].y)) }
            c.strokePath(); return
        }
        for k in from..<to {
            switch Geometry.piece(pts, k, s.brush.width, s.brush.pressure) {
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
#endif
