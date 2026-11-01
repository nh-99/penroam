#if canImport(UIKit)
import InkCore
import PencilInk
import PencilKit
import UIKit

/// Standalone UIKit/PencilKit reference host (M00.6A) built on the shared
/// PencilInk backend, plus the on-device interoperability/fidelity tests (M00.8).
final class PencilHostView: UIView {
    let backend = PencilInkBackend()
    var onChange: (() -> Void)?

    override init(frame: CGRect) {
        super.init(frame: frame)
        backgroundColor = .white
        backend.attachSurface(to: self, frame: bounds)
        backend.canvas.autoresizingMask = [.flexibleWidth, .flexibleHeight]
        backend.onStrokesCommitted = { [weak self] _ in self?.onChange?() }
        backend.onStrokesErased = { [weak self] _ in self?.onChange?() }
    }

    required init?(coder: NSCoder) { fatalError() }

    override func layoutSubviews() {
        super.layoutSubviews()
        backend.setFrame(bounds)
    }

    // MARK: exports

    /// Canonical document + native PKDrawing (base64) for fidelity comparison.
    func exportBoth() throws -> (canonical: String, native: String) {
        let strokes = backend.exportStrokes()
        let doc = InkDocument(strokes: strokes, meta: ["app": .string("ios-pencilkit"), "units": .string("points")])
        let canonical = String(decoding: try JSONEncoder().encode(doc), as: UTF8.self)
        let stamp = Export.stamp()
        let p1 = try Export.save(dir: "strokes", name: "pencilkit-\(stamp).json", text: canonical, log: false)
        let p2 = try Export.save(dir: "strokes", name: "pencilkit-\(stamp).pkdrawing.b64", text: backend.nativeDrawingData().base64EncodedString(), log: false)
        return (p1, p2)
    }

    func importCanonical(_ data: Data) throws -> Int {
        let doc = try JSONDecoder().decode(InkDocument.self, from: data)
        backend.loadStrokes(doc.strokes)
        onChange?()
        return doc.strokes.count
    }

    // MARK: interoperability / fidelity test (automated, on device)

    /// Runs on a separate, offscreen backend so the user's drawing is untouched.
    func runInteropTest(fixtures: [String: [InkStroke]]) -> [String: Any] {
        var out: [String: Any] = ["app": "ios-pencilkit-interop", "exportedAt": ISO8601DateFormatter().string(from: Date()),
                                  "system": Export.systemInfo()]
        var cases: [[String: Any]] = []

        // 1. canonical -> PencilKit -> canonical (fixtures)
        for (name, strokes) in fixtures.sorted(by: { $0.key < $1.key }) {
            let b = PencilInkBackend()
            let importMs = b.loadStrokes(strokes)
            let t0 = CACurrentMediaTime()
            let back = b.exportStrokes()
            let exportMs = (CACurrentMediaTime() - t0) * 1000
            let r = StrokeCompare.compare(strokes, back)
            // Same drawing, but canonical data rebuilt from the PKDrawing alone (no session brush registry).
            let rNative = StrokeCompare.compare(strokes, b.exportStrokesFromNativeOnly())
            // native serialization round trip
            let data = b.nativeDrawingData()
            let reread = (try? PKDrawing(data: data))
            // PKDrawing encoding is not byte-stable; compare decoded content instead.
            let nativeOK = reread.map { r in
                r.strokes.count == strokes.count &&
                StrokeCompare.compare(zip(r.strokes, strokes).map { PencilConvert.toCanonical($0, id: $1.id, nominalWidth: nil) }, back).maxPositionError < 1e-9
            } ?? false
            // visual: shared-geometry render vs PencilKit render of the imported strokes
            let rect = bounds(of: strokes)
            let vis = Fidelity.compare(Fidelity.renderCanonical(strokes, rect: rect), Fidelity.renderPencil(b.canvas.drawing, rect: rect))
            cases.append(["case": "canonical->pencilkit->canonical:\(name)", "importMs": importMs, "exportMs": exportMs,
                          "geometry": encode(r), "geometry_fromNativeOnly": encode(rNative), "nativeDataRoundTripEquivalent": nativeOK, "nativeBytesStableOnReencode": reread?.dataRepresentation() == data, "nativeBytes": data.count,
                          "visual_canonicalRender_vs_pencilRender": vis,
                          "pencilBoundsVsCanonicalBounds": ["pencil": NSCoder.string(for: b.canvas.drawing.bounds), "canonical": NSCoder.string(for: rect)]])
        }

        // 2. user's live PencilKit drawing -> canonical -> PencilKit
        let live = backend.canvas.drawing
        if !live.strokes.isEmpty {
            let canon = backend.exportStrokes()
            let json = (try? JSONEncoder().encode(InkDocument(strokes: canon))) ?? Data()
            let decoded = (try? JSONDecoder().decode(InkDocument.self, from: json))?.strokes ?? []
            let b = PencilInkBackend()
            b.loadStrokes(decoded)
            let again = b.exportStrokes()
            let rect = live.bounds.insetBy(dx: -8, dy: -8)
            cases.append([
                "case": "pencilkit(live)->canonical->pencilkit",
                "strokes": live.strokes.count, "canonicalBytes": json.count, "nativeBytes": live.dataRepresentation().count,
                "geometry_canonical_vs_reexport": encode(StrokeCompare.compare(decoded, again)),
                "visual_original_vs_reimported": Fidelity.compare(Fidelity.renderPencil(live, rect: rect), Fidelity.renderPencil(b.canvas.drawing, rect: rect)),
                "visual_original_vs_canonicalRender": Fidelity.compare(Fidelity.renderPencil(live, rect: rect), Fidelity.renderCanonical(decoded, rect: rect)),
                "observedForceRange": backend.getCapabilities()["observedForceRange"] ?? [],
                "pkSizeMedianVsNominal": canon.prefix(20).map { s -> [Double] in
                    if case .number(let m)? = s.input["pkSizeMedian"] { return [m, s.brush.width] }; return []
                },
            ])
        } else {
            cases.append(["case": "pencilkit(live)->canonical->pencilkit", "skipped": "no strokes drawn in the PencilKit engine yet"])
        }
        out["cases"] = cases
        return out
    }

    private func bounds(of strokes: [InkStroke]) -> CGRect {
        var r = CGRect.null
        for s in strokes { let b = s.bounds; r = r.union(CGRect(x: b[0], y: b[1], width: b[2] - b[0], height: b[3] - b[1])) }
        return r.isNull ? CGRect(x: 0, y: 0, width: 10, height: 10) : r.insetBy(dx: -4, dy: -4).integral
    }

    private func encode<T: Encodable>(_ v: T) -> Any {
        (try? JSONSerialization.jsonObject(with: JSONEncoder().encode(v))) ?? [:]
    }
}
#endif
