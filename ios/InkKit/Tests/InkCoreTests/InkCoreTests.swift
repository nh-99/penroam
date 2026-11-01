import Foundation
import XCTest
@testable import InkCore

final class InkCoreTests: XCTestCase {
    func fixture(_ p: String) throws -> Data {
        // Tests/InkCoreTests/ -> repo root/fixtures
        let here = URL(fileURLWithPath: #filePath)
        let url = here.deletingLastPathComponent().appendingPathComponent("../../../../tests/fixtures/\(p)").standardized
        return try Data(contentsOf: url)
    }

    func testWidthAndPiecesMatchGolden() throws {
        let golden = try JSONSerialization.jsonObject(with: fixture("geometry-golden.json")) as! [String: Any]
        for c in golden["widthCases"] as! [[String: Any]] {
            let w = Geometry.widthFor((c["base"] as! NSNumber).doubleValue, (c["pressure"] as! NSNumber).doubleValue, c["pressureEnabled"] as! Bool)
            XCTAssertEqual(w, (c["width"] as! NSNumber).doubleValue, accuracy: 1e-12)
        }
        let doc = try JSONDecoder().decode(InkDocument.self, from: fixture("sample-strokes.json"))
        let byId = Dictionary(uniqueKeysWithValues: doc.strokes.map { ($0.id, $0) })
        for g in golden["strokes"] as! [[String: Any]] {
            let s = byId[g["id"] as! String]!
            let pieces = Geometry.pieces(s.points, s.brush.width, s.brush.pressure)
            let exp = g["pieces"] as! [[String: Any]]
            XCTAssertEqual(pieces.count, exp.count, s.id)
            for (p, e) in zip(pieces, exp) {
                func v(_ k: String) -> Double { (e[k] as! NSNumber).doubleValue }
                XCTAssertEqual(p.width, v("width"), accuracy: 1e-9)
                switch p {
                case let .dot(x, y, _): XCTAssertEqual(e["kind"] as? String, "dot"); XCTAssertEqual(x, v("x"), accuracy: 1e-9); XCTAssertEqual(y, v("y"), accuracy: 1e-9)
                case let .line(x0, y0, x1, y1, _):
                    XCTAssertEqual(e["kind"] as? String, "line")
                    for (a, k) in [(x0, "x0"), (y0, "y0"), (x1, "x1"), (y1, "y1")] { XCTAssertEqual(a, v(k), accuracy: 1e-9) }
                case let .quad(x0, y0, cx, cy, x1, y1, _):
                    XCTAssertEqual(e["kind"] as? String, "quad")
                    for (a, k) in [(x0, "x0"), (y0, "y0"), (cx, "cx"), (cy, "cy"), (x1, "x1"), (y1, "y1")] { XCTAssertEqual(a, v(k), accuracy: 1e-9) }
                }
            }
        }
    }

    func testScenesMatchFixtures() throws {
        for n in [100, 1000] {
            let expected = try JSONDecoder().decode(InkDocument.self, from: fixture("benchmark-scenes/scene-\(n).json")).strokes
            let actual = Scene.generate(n)
            XCTAssertEqual(actual.count, expected.count)
            for (a, e) in zip(actual, expected) {
                XCTAssertEqual(a.id, e.id)
                XCTAssertEqual(a.brush.width, e.brush.width)
                XCTAssertEqual(a.points, e.points, a.id)
            }
        }
    }

    func testEraserAndMetrics() throws {
        let doc = try JSONDecoder().decode(InkDocument.self, from: fixture("sample-strokes.json"))
        let line = doc.strokes.first { $0.id == "sample-line" }!
        XCTAssertTrue(Geometry.hitTest(line, 300, 130, 4))
        XCTAssertFalse(Geometry.hitTest(line, 300, 200, 4))
        let s = Metrics.summarize((1...100).map(Double.init))
        XCTAssertEqual(s["median"], 51); XCTAssertEqual(s["p95"], 95)
        let g = analyzeSamples([0, 4, 8, 200, 204], [0, 1, 2, 3, 300], [0, 0, 0, 0, 0])
        XCTAssertEqual(g["timeGaps"], 1); XCTAssertEqual(g["spatialJumps"], 1)
    }
}
