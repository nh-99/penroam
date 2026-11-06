import XCTest
import InkCore
@testable import PencilSurface

final class PencilSurfaceTests: XCTestCase {
    private func s(_ x: Double, _ y: Double, _ f: Double, _ t: Double, est: Int? = nil, expect: Bool = false) -> PencilSample {
        PencilSample(x: x, y: y, force: f, maximumForce: 4.17, timestamp: t, estimationIndex: est, expectsForceUpdate: expect)
    }

    func testCoalescedSamplesMapToDocumentSpaceInOrder() {
        let st = LiveStroke(brush: Brush(), transform: ViewTransform(scale: 2, offsetX: 20, offsetY: 10))
        st.add([s(20, 10, 2.085, 100.000), s(22, 12, 2.085, 100.004), s(24, 14, 4.17, 100.008)], coalesced: true)
        XCTAssertEqual(st.points.count, 3)
        XCTAssertEqual(st.points[0].x, 0); XCTAssertEqual(st.points[0].y, 0)
        XCTAssertEqual(st.points[2].x, 2); XCTAssertEqual(st.points[2].y, 2)
        XCTAssertEqual(st.points[0].pressure, NativeMapping.pencilPressure(fraction: 0.5), accuracy: 1e-4)
        XCTAssertEqual(st.points[2].pressure, 1)
        XCTAssertEqual(st.points[1].timeUs, 4000)
        XCTAssertEqual(st.coalescedCount, 3)
        // out-of-order (older) samples are ignored
        st.add([s(30, 30, 1, 100.002)], coalesced: false)
        XCTAssertEqual(st.points.count, 3)
    }

    func testEstimatedForceUpdatesPatchTheRecordedPoint() {
        let st = LiveStroke(brush: Brush(), transform: ViewTransform())
        st.add([s(0, 0, 1, 1, est: 7, expect: true), s(5, 5, 1, 1.01)], coalesced: false)
        XCTAssertTrue(st.updateEstimated(index: 7, force: 3.0, maximumForce: 4.17, final: true))
        XCTAssertEqual(st.points[0].pressure, (NativeMapping.pencilPressure(fraction: 3.0 / 4.17) * 10000).rounded() / 10000)
        XCTAssertFalse(st.updateEstimated(index: 7, force: 1, maximumForce: 4.17, final: true), "final update consumed")
        let json = st.finish()
        let pts = json["points"] as! [[String: Any]]
        XCTAssertEqual(pts[0]["flags"] as? Int, PointFlags.estimatedPressure)
        XCTAssertNil(pts[1]["flags"])
        XCTAssertEqual((json["input"] as! [String: Any])["estimatedForceUpdates"] as? Int, 1)
    }

    func testPredictedSamplesAreNeverCommitted() {
        let st = LiveStroke(brush: Brush(), transform: ViewTransform())
        st.add([s(0, 0, 1, 1), s(1, 1, 1, 1.004)], coalesced: false)
        st.setPredicted([s(2, 2, 1, 1.008), s(3, 3, 1, 1.012)])
        XCTAssertEqual(st.predicted.count, 2)
        let pts = st.finish()["points"] as! [[String: Any]]
        XCTAssertEqual(pts.count, 2)
        XCTAssertTrue(st.predicted.isEmpty)
    }

    func testHighlighterIsFixedWidthAndTranslucent() {
        let st = LiveStroke(brush: Brush(tool: "highlighter", color: "#facc15", width: 14, pressure: true, opacity: 0.35), transform: ViewTransform())
        st.add([s(0, 0, 1, 1)], coalesced: false)
        let b = st.finish()["brush"] as! [String: Any]
        XCTAssertEqual(b["pressure"] as? Bool, false)
        XCTAssertEqual(b["opacity"] as? Double, 0.35)
    }

    func testHandoffKeepsPreviewUntilAckOrTimeout() {
        let q = HandoffQueue()
        q.finished("a", at: 10)
        q.finished("b", at: 10.5)
        XCTAssertTrue(q.ack("a"))
        XCTAssertFalse(q.ack("a"), "double ack is harmless")
        XCTAssertEqual(q.expire(now: 11, timeout: 2), [])
        XCTAssertEqual(q.expire(now: 13, timeout: 2), ["b"])
        XCTAssertTrue(q.pending.isEmpty)
    }

    func testOverlayFrameFollowsScrollAndZoom() {
        let f = OverlayFrame.frame(cssX: 100, cssY: 80, cssW: 500, cssH: 700, zoom: 1, contentOffsetX: 0, contentOffsetY: -24)
        XCTAssertEqual(f.x, 100); XCTAssertEqual(f.y, 104); XCTAssertEqual(f.width, 500); XCTAssertEqual(f.height, 700)
        // a doc point lands on the same spot natively and in the web canvas
        let t = ViewTransform(scale: 1.5, offsetX: -12, offsetY: 30)
        let (vx, vy) = t.toView(200, 300)
        let (dx, dy) = t.toDoc(vx, vy)
        XCTAssertEqual(dx, 200, accuracy: 1e-9); XCTAssertEqual(dy, 300, accuracy: 1e-9)
    }
}

/// The fixture is shared with the web and Kotlin tests, so all platforms place ink identically.
final class ViewportVectorTests: XCTestCase {
    private struct Vectors: Decodable {
        struct Case: Decodable { let scale, ox, oy, dpr: Double; let doc, view: [Double] }
        let cases: [Case]
    }

    func testDocumentPointsLandOnTheSharedPoints() throws {
        var dir = URL(fileURLWithPath: #filePath)
        while !FileManager.default.fileExists(atPath: dir.appendingPathComponent("tests/fixtures/viewport-vectors.json").path) {
            dir.deleteLastPathComponent()
        }
        let data = try Data(contentsOf: dir.appendingPathComponent("tests/fixtures/viewport-vectors.json"))
        let vectors = try JSONDecoder().decode(Vectors.self, from: data)
        XCTAssertFalse(vectors.cases.isEmpty)
        for c in vectors.cases {
            // UIKit points are CSS px, so the surface works before the display scale
            let t = ViewTransform(scale: c.scale, offsetX: c.ox, offsetY: c.oy)
            let (x, y) = t.toView(c.doc[0], c.doc[1])
            XCTAssertEqual(x * c.dpr, c.view[0], accuracy: 1e-3)
            XCTAssertEqual(y * c.dpr, c.view[1], accuracy: 1e-3)
            let (dx, dy) = t.toDoc(c.view[0] / c.dpr, c.view[1] / c.dpr)
            XCTAssertEqual(dx, c.doc[0], accuracy: 1e-3)
            XCTAssertEqual(dy, c.doc[1], accuracy: 1e-3)
        }
    }

    func testFinishedStrokeKeepsTheForceScale() {
        let s = LiveStroke(brush: Brush(), transform: ViewTransform())
        s.add([PencilSample(x: 1, y: 1, force: 2, maximumForce: 4, timestamp: 0)], coalesced: false)
        let input = s.finish()["input"] as? [String: Any]
        XCTAssertEqual(input?["maximumForce"] as? Double, 4)
    }

    func testPencilCurveMatchesTheWebEditorAndCanBeInverted() {
        XCTAssertEqual(NativeMapping.pencilPressure(fraction: 0), 0)
        XCTAssertEqual(NativeMapping.pencilPressure(fraction: 1), 1, accuracy: 1e-9)
        XCTAssertEqual(NativeMapping.pencilPressure(fraction: 0.111), 0.404, accuracy: 0.01)
        XCTAssertEqual(NativeMapping.pencilPressure(fraction: 0.218), 0.639, accuracy: 0.01)
        for f in stride(from: 0.0, through: 1.0, by: 0.05) {
            XCTAssertEqual(NativeMapping.pencilFraction(pressure: NativeMapping.pencilPressure(fraction: f)), f, accuracy: 1e-9)
        }
    }
}
