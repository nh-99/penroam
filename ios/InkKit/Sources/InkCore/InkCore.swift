// Swift port of the shared ink model, geometry, scene generator and
// metrics. Reference: fixtures/generate-fixtures.mjs. Verified by
// Tests/InkCoreTests against fixtures/geometry-golden.json and scene-100/1000.
import Foundation

// MARK: - Model

public struct InkPoint: Codable, Equatable, Sendable {
    public var x: Double
    public var y: Double
    public var pressure: Double
    public var tiltX: Double
    public var tiltY: Double
    public var timeUs: Int64

    public init(x: Double, y: Double, pressure: Double, tiltX: Double = 0, tiltY: Double = 0, timeUs: Int64 = 0) {
        self.x = x; self.y = y; self.pressure = pressure; self.tiltX = tiltX; self.tiltY = tiltY; self.timeUs = timeUs
    }
}

public struct InkBrush: Codable, Equatable, Sendable {
    public var type: String = "pen"
    public var width: Double = 3
    public var color: String = "#000000"
    public var pressure: Bool = true
    public init(width: Double = 3, pressure: Bool = true) { self.width = width; self.pressure = pressure }
}

public final class InkStroke: Codable {
    public let id: String
    public let source: String
    public var input: [String: JSONValue]
    public let brush: InkBrush
    public var points: [InkPoint]
    private var cachedBounds: [Double]?

    enum CodingKeys: String, CodingKey { case id, source, input, brush, points }

    public init(id: String = UUID().uuidString.lowercased(), source: String, input: [String: JSONValue], brush: InkBrush, points: [InkPoint]) {
        self.id = id; self.source = source; self.input = input; self.brush = brush; self.points = points
    }

    public required init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = (try? c.decode(String.self, forKey: .id)) ?? UUID().uuidString.lowercased()
        source = (try? c.decode(String.self, forKey: .source)) ?? "unknown"
        input = (try? c.decode([String: JSONValue].self, forKey: .input)) ?? [:]
        brush = (try? c.decode(InkBrush.self, forKey: .brush)) ?? InkBrush()
        points = try c.decode([InkPoint].self, forKey: .points)
    }

    /// [minX, minY, maxX, maxY] including half the max width (computed once; call after points are final).
    public var bounds: [Double] {
        if let b = cachedBounds { return b }
        var minX = Double.infinity, minY = Double.infinity, maxX = -Double.infinity, maxY = -Double.infinity, maxW = 0.0
        for p in points {
            minX = min(minX, p.x); minY = min(minY, p.y); maxX = max(maxX, p.x); maxY = max(maxY, p.y)
            maxW = max(maxW, Geometry.widthFor(brush.width, p.pressure, brush.pressure))
        }
        let pad = maxW / 2 + 1
        let b = [minX - pad, minY - pad, maxX + pad, maxY + pad]
        cachedBounds = b
        return b
    }
}

/// Minimal JSON value for the free-form `input` diagnostics map.
public enum JSONValue: Codable, Equatable, Sendable {
    case string(String), number(Double), bool(Bool), null

    public init(from decoder: Decoder) throws {
        let c = try decoder.singleValueContainer()
        if c.decodeNil() { self = .null }
        else if let b = try? c.decode(Bool.self) { self = .bool(b) }
        else if let d = try? c.decode(Double.self) { self = .number(d) }
        else if let s = try? c.decode(String.self) { self = .string(s) }
        else { self = .null }
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.singleValueContainer()
        switch self {
        case .string(let s): try c.encode(s)
        case .number(let d): try c.encode(d)
        case .bool(let b): try c.encode(b)
        case .null: try c.encodeNil()
        }
    }
}

public struct InkDocument: Codable {
    public var format = "m00-strokes"
    public var version = 1
    public var meta: [String: JSONValue] = [:]
    public var strokes: [InkStroke]
    public init(strokes: [InkStroke], meta: [String: JSONValue] = [:]) { self.strokes = strokes; self.meta = meta }

    enum CodingKeys: String, CodingKey { case format, version, meta, strokes }
    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        strokes = try c.decode([InkStroke].self, forKey: .strokes)
        meta = (try? c.decode([String: JSONValue].self, forKey: .meta)) ?? [:]
    }
}

// MARK: - Geometry

public enum Piece: Equatable, Sendable {
    case dot(x: Double, y: Double, width: Double)
    case line(x0: Double, y0: Double, x1: Double, y1: Double, width: Double)
    case quad(x0: Double, y0: Double, cx: Double, cy: Double, x1: Double, y1: Double, width: Double)

    public var width: Double {
        switch self {
        case .dot(_, _, let w), .line(_, _, _, _, let w), .quad(_, _, _, _, _, _, let w): return w
        }
    }
}

public enum Geometry {
    public static func widthFor(_ base: Double, _ pressure: Double, _ enabled: Bool) -> Double {
        guard enabled else { return base }
        let p = min(1, max(0, pressure))
        return max(0.5, base * (0.4 + 1.2 * p))
    }

    public static func pieceCount(_ n: Int) -> Int { n == 0 ? 0 : (n <= 2 ? 1 : n) }

    /// Pieces that cannot change once n points are known (all but the trailing line).
    public static func stablePieceCount(_ n: Int) -> Int { n < 3 ? 0 : n - 1 }

    public static func piece(_ pts: [InkPoint], _ k: Int, _ base: Double, _ pe: Bool) -> Piece {
        let n = pts.count
        func w(_ p: InkPoint) -> Double { widthFor(base, p.pressure, pe) }
        if n == 1 { return .dot(x: pts[0].x, y: pts[0].y, width: w(pts[0])) }
        if n == 2 { return .line(x0: pts[0].x, y0: pts[0].y, x1: pts[1].x, y1: pts[1].y, width: w(pts[0])) }
        if k == 0 {
            let a = pts[0], b = pts[1]
            return .line(x0: a.x, y0: a.y, x1: (a.x + b.x) / 2, y1: (a.y + b.y) / 2, width: w(a))
        }
        if k == n - 1 {
            let a = pts[n - 2], b = pts[n - 1]
            return .line(x0: (a.x + b.x) / 2, y0: (a.y + b.y) / 2, x1: b.x, y1: b.y, width: w(b))
        }
        let p0 = pts[k - 1], p1 = pts[k], p2 = pts[k + 1]
        return .quad(x0: (p0.x + p1.x) / 2, y0: (p0.y + p1.y) / 2, cx: p1.x, cy: p1.y,
                     x1: (p1.x + p2.x) / 2, y1: (p1.y + p2.y) / 2, width: w(p1))
    }

    public static func pieces(_ pts: [InkPoint], _ base: Double, _ pe: Bool) -> [Piece] {
        (0..<pieceCount(pts.count)).map { piece(pts, $0, base, pe) }
    }

    public static func hitTest(_ s: InkStroke, _ cx: Double, _ cy: Double, _ r: Double) -> Bool {
        let b = s.bounds
        if cx < b[0] - r || cx > b[2] + r || cy < b[1] - r || cy > b[3] + r { return false }
        let pts = s.points, half = s.brush.width / 2
        if pts.count == 1 { return hypot(cx - pts[0].x, cy - pts[0].y) <= r + half }
        for i in 1..<pts.count where segDist(cx, cy, pts[i - 1].x, pts[i - 1].y, pts[i].x, pts[i].y) <= r + half {
            return true
        }
        return false
    }

    static func segDist(_ px: Double, _ py: Double, _ ax: Double, _ ay: Double, _ bx: Double, _ by: Double) -> Double {
        let dx = bx - ax, dy = by - ay, len2 = dx * dx + dy * dy
        if len2 == 0 { return hypot(px - ax, py - ay) }
        let t = min(1, max(0, ((px - ax) * dx + (py - ay) * dy) / len2))
        return hypot(px - (ax + t * dx), py - (ay + t * dy))
    }
}

// MARK: - Scene generator (must match fixtures/benchmark-scenes exactly)

public enum Scene {
    public static func mulberry32(_ seed: UInt32) -> () -> Double {
        var a = seed
        return {
            a = a &+ 0x6D2B_79F5
            var t = a
            t = (t ^ (t >> 15)) &* (t | 1)
            t ^= t &+ ((t ^ (t >> 7)) &* (t | 61))
            return Double(t ^ (t >> 14)) / 4_294_967_296.0
        }
    }

    // JS Math.round == floor(x + 0.5); all values here are >= 0.
    static func round2(_ v: Double) -> Double { (v * 100 + 0.5).rounded(.down) / 100 }
    static func round3(_ v: Double) -> Double { (v * 1000 + 0.5).rounded(.down) / 1000 }

    public static func generate(_ count: Int) -> [InkStroke] {
        let rnd = mulberry32(UInt32(count))
        let W = 1600.0, H = 1200.0, widths = [1.5, 3.0, 6.0]
        var out: [InkStroke] = []
        out.reserveCapacity(count)
        for i in 0..<count {
            var x = 40 + rnd() * (W - 80)
            var y = 40 + rnd() * (H - 80)
            let n = 8 + Int((rnd() * 40).rounded(.down))
            var vx = (rnd() - 0.5) * 6
            var vy = (rnd() - 0.5) * 6
            var p = 0.3 + 0.5 * rnd()
            let width = widths[Int((rnd() * 3).rounded(.down))]
            var pts: [InkPoint] = []
            pts.reserveCapacity(n)
            for j in 0..<n {
                if j > 0 {
                    vx = vx * 0.85 + (rnd() - 0.5) * 3
                    vy = vy * 0.85 + (rnd() - 0.5) * 3
                    x = min(W, max(0, x + vx))
                    y = min(H, max(0, y + vy))
                    p = p * 0.7 + 0.3 * (0.2 + 0.7 * rnd())
                }
                pts.append(InkPoint(x: round2(x), y: round2(y), pressure: round3(p), timeUs: Int64(j * 4000)))
            }
            out.append(InkStroke(id: "scene\(count)-\(i)", source: "fixture",
                                 input: ["api": .string("generated"), "device": .string("none")],
                                 brush: InkBrush(width: width), points: pts))
        }
        return out
    }
}

// MARK: - Metrics (names match docs/benchmarks/protocol.md)

public final class Metrics {
    public private(set) var counters: [String: Int] = [:]
    private var series: [String: (values: [Double], count: Int, next: Int)] = [:]
    public private(set) var events: [[String: JSONValue]] = []
    public private(set) var strokes: [[String: JSONValue]] = []
    public private(set) var errors: [String] = []
    public var info: [String: JSONValue] = [:]
    let cap: Int
    public init(cap: Int = 8192) { self.cap = cap }

    public func count(_ name: String, _ d: Int = 1) { counters[name, default: 0] += d }

    public func timing(_ name: String, _ ms: Double) {
        var s = series[name] ?? ([], 0, 0)
        if s.values.count < cap { s.values.append(ms) } else { s.values[s.next] = ms }
        s.next = (s.next + 1) % cap
        s.count += 1
        series[name] = s
    }

    public func event(_ name: String, _ detail: [String: JSONValue] = [:]) {
        count(name)
        var e = detail
        e["name"] = .string(name)
        e["t"] = .number(ProcessInfo.processInfo.systemUptime * 1000)
        events.append(e)
        if events.count > 512 { events.removeFirst() }
    }

    public func error(_ m: String) { errors.append(m); if errors.count > 64 { errors.removeFirst() } }
    public func stroke(_ s: [String: JSONValue]) { strokes.append(s); if strokes.count > 512 { strokes.removeFirst() } }

    public func get(_ name: String) -> [String: Double]? {
        guard let s = series[name] else { return nil }
        return Metrics.summarize(s.values, total: s.count)
    }

    public func reset() { counters = [:]; series = [:]; events = []; strokes = []; errors = [] }

    public static func summarize(_ values: [Double], total: Int? = nil) -> [String: Double] {
        var out: [String: Double] = ["count": Double(total ?? values.count), "retained": Double(values.count)]
        guard !values.isEmpty else { return out }
        let s = values.sorted()
        func pct(_ p: Double) -> Double {
            // Same index rule as JS Math.round((n-1)*p).
            s[Int((Double(s.count - 1) * p + 0.5).rounded(.down))]
        }
        out["median"] = pct(0.5); out["p95"] = pct(0.95); out["max"] = s.last!; out["min"] = s.first!
        out["mean"] = s.reduce(0, +) / Double(s.count)
        return out
    }

    public func toJSON() -> [String: Any] {
        var timings: [String: Any] = [:]
        for (k, s) in series { timings[k] = Metrics.summarize(s.values, total: s.count) }
        let enc = { (v: Any) -> Any in
            (try? JSONSerialization.jsonObject(with: JSONEncoder().encode(AnyEncodable(v)))) ?? NSNull()
        }
        return ["clock": "ProcessInfo.systemUptime / UITouch.timestamp (same clock)", "counters": counters,
                "timings": timings, "events": enc(events), "strokes": enc(strokes), "errors": errors, "info": enc(info)]
    }
}

struct AnyEncodable: Encodable {
    let v: Any
    init(_ v: Any) { self.v = v }
    func encode(to encoder: Encoder) throws {
        if let x = v as? [[String: JSONValue]] { try x.encode(to: encoder) }
        else if let x = v as? [String: JSONValue] { try x.encode(to: encoder) }
        else { var c = encoder.singleValueContainer(); try c.encodeNil() }
    }
}

/// Gap analysis on delivered sample timestamps (same rule as Kotlin/TS/Dart).
public func analyzeSamples(_ ts: [Double], _ xs: [Double], _ ys: [Double]) -> [String: Double] {
    let dts = ts.count > 1 ? (1..<ts.count).map { ts[$0] - ts[$0 - 1] } : []
    let sorted = dts.sorted()
    let median = sorted.isEmpty ? 0 : sorted[sorted.count / 2]
    let gapThreshold = max(30, median * 4)
    var gaps = 0, jumps = 0
    for i in 0..<dts.count {
        if dts[i] > gapThreshold { gaps += 1 }
        if hypot(xs[i + 1] - xs[i], ys[i + 1] - ys[i]) > 40 { jumps += 1 }
    }
    return ["medianDtMs": median, "maxDtMs": sorted.last ?? 0, "timeGaps": Double(gaps), "spatialJumps": Double(jumps)]
}
