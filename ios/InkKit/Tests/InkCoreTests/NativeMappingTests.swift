import Foundation
import XCTest
@testable import InkCore

final class NativeMappingTests: XCTestCase {
    func testTiltRoundTrip() {
        for (tx, ty) in [(0.0, 0.0), (30.0, 0.0), (0.0, -45.0), (20.0, 35.0), (-60.0, 10.0)] {
            let aa = NativeMapping.altitudeAzimuth(tiltX: tx, tiltY: ty)
            let back = NativeMapping.tilt(altitude: aa.altitude, azimuth: aa.azimuth)
            XCTAssertEqual(back.x, tx, accuracy: 1e-9); XCTAssertEqual(back.y, ty, accuracy: 1e-9)
        }
        let perp = NativeMapping.altitudeAzimuth(tiltX: 0, tiltY: 0)
        XCTAssertEqual(perp.altitude, .pi / 2, accuracy: 1e-12)
    }

    func testForcePressureRoundTrip() {
        for p in stride(from: 0.0, through: 1.0, by: 0.05) {
            XCTAssertEqual(NativeMapping.pressure(fromForce: NativeMapping.force(fromPressure: p)), p, accuracy: 1e-12)
        }
        XCTAssertEqual(NativeMapping.pressure(fromForce: 99), 1)
    }

    func testInvertWidthWithQuantizedPressure() {
        // Sizes from the true pressure; inversion sees pressure perturbed like PencilKit force quantization.
        for base in [1.5, 3.0, 6.0, 2.25] {
            let ps = [0.12, 0.4, 0.55, 0.81, 0.97, 0.6]
            let sizes = ps.map { Geometry.widthFor(base, $0, true) }
            let seen = ps.enumerated().map { $0.element + ($0.offset % 2 == 0 ? 2e-4 : -2e-4) }
            let (w, on) = NativeMapping.invertWidth(sizes: sizes, pressures: seen)
            XCTAssertEqual(w, base, accuracy: 1e-9); XCTAssertTrue(on)
        }
        // Pressure-off stroke: constant size while pressure varies.
        let (w, on) = NativeMapping.invertWidth(sizes: [3, 3, 3], pressures: [0.2, 0.5, 0.9])
        XCTAssertEqual(w, 3); XCTAssertFalse(on)
    }

    func testCompareDetectsDifferences() {
        let a = InkStroke(id: "a", source: "s", input: [:], brush: InkBrush(width: 3), points: [InkPoint(x: 0, y: 0, pressure: 0.5), InkPoint(x: 10, y: 0, pressure: 0.5)])
        let b = InkStroke(id: "b", source: "s", input: [:], brush: InkBrush(width: 3), points: [InkPoint(x: 0, y: 0, pressure: 0.6), InkPoint(x: 10, y: 1, pressure: 0.5)])
        let r = StrokeCompare.compare([a], [b])
        XCTAssertEqual(r.maxPositionError, 1, accuracy: 1e-12)
        XCTAssertEqual(r.maxPressureError, 0.1, accuracy: 1e-12)
        XCTAssertTrue(r.orderPreserved)
    }

    /// Canonical fixtures survive JSON encode/decode with InkCore's Codable types.
    func testCanonicalJsonRoundTrip() throws {
        let url = URL(fileURLWithPath: #filePath).deletingLastPathComponent().appendingPathComponent("../../../../tests/fixtures/sample-strokes.json").standardized
        let doc = try JSONDecoder().decode(InkDocument.self, from: Data(contentsOf: url))
        let again = try JSONDecoder().decode(InkDocument.self, from: JSONEncoder().encode(doc))
        let r = StrokeCompare.compare(doc.strokes, again.strokes)
        XCTAssertEqual(r.maxPositionError, 0); XCTAssertEqual(r.pointCountMismatches, 0)
    }
}
