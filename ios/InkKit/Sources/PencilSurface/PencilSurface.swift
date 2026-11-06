// Penroam iPad native ink surface: platform-independent core.
//
// The UIKit layer (plugins/tauri-plugin-native-ink/ios) feeds raw Pencil
// samples into `LiveStroke`; this file holds everything that can be tested
// without UIKit (and therefore on Linux with `swift test`):
//
//  * sample -> canonical document point mapping (same transform as the web
//    canvas: screen = doc * scale + offset, in CSS px == UIKit points),
//  * coalesced samples appended in order, predicted samples kept separately
//    and never committed,
//  * estimated force updates patched into already-recorded points by
//    `estimationUpdateIndex` (flagged in `flags`),
//  * the commit hand-off: a finished stroke stays drawn natively until the web
//    renderer acknowledges it painted the committed copy (or a timeout).
import Foundation
import InkCore

public struct ViewTransform: Equatable, Sendable {
    public var scale: Double
    public var offsetX: Double
    public var offsetY: Double
    public init(scale: Double = 1, offsetX: Double = 0, offsetY: Double = 0) {
        self.scale = scale; self.offsetX = offsetX; self.offsetY = offsetY
    }
    public func toDoc(_ x: Double, _ y: Double) -> (Double, Double) { ((x - offsetX) / scale, (y - offsetY) / scale) }
    public func toView(_ x: Double, _ y: Double) -> (Double, Double) { (x * scale + offsetX, y * scale + offsetY) }
}

/// One Pencil sample in surface-local points.
public struct PencilSample: Sendable {
    public var x: Double, y: Double
    public var force: Double, maximumForce: Double
    public var altitude: Double, azimuth: Double
    public var timestamp: Double // seconds (UITouch.timestamp)
    public var estimationIndex: Int? // UITouch.estimationUpdateIndex
    public var expectsForceUpdate: Bool
    public init(x: Double, y: Double, force: Double, maximumForce: Double, altitude: Double = .pi / 2, azimuth: Double = 0,
                timestamp: Double, estimationIndex: Int? = nil, expectsForceUpdate: Bool = false) {
        self.x = x; self.y = y; self.force = force; self.maximumForce = maximumForce
        self.altitude = altitude; self.azimuth = azimuth; self.timestamp = timestamp
        self.estimationIndex = estimationIndex; self.expectsForceUpdate = expectsForceUpdate
    }
}

public struct Brush: Equatable, Sendable {
    public var tool: String // "pen" | "highlighter"
    public var color: String
    public var width: Double
    public var pressure: Bool
    public var opacity: Double
    public init(tool: String = "pen", color: String = "#111111", width: Double = 2.2, pressure: Bool = true, opacity: Double = 1) {
        self.tool = tool; self.color = color; self.width = width; self.pressure = pressure; self.opacity = opacity
    }
}

public enum PointFlags {
    public static let estimatedPressure = 1
    public static let coalesced = 2
}

/// A stroke being drawn natively.
public final class LiveStroke {
    public let id: String
    public let brush: Brush
    public let transform: ViewTransform
    public private(set) var points: [InkPoint] = []
    public private(set) var flags: [Int] = []
    public private(set) var predicted: [InkPoint] = []
    public private(set) var coalescedCount = 0
    public private(set) var estimatedUpdates = 0
    /// The Pencil's full force; raw force = `pencilFraction(pressure) * this`.
    public private(set) var maximumForce = 0.0
    private var t0: Double?
    private var pendingEstimates: [Int: Int] = [:] // estimationIndex -> point index
    private let r3 = { (v: Double) in (v * 1000).rounded() / 1000 }

    public init(id: String = UUID().uuidString.lowercased(), brush: Brush, transform: ViewTransform) {
        self.id = id; self.brush = brush; self.transform = transform
    }

    private func point(_ s: PencilSample) -> InkPoint {
        if t0 == nil { t0 = s.timestamp }
        maximumForce = max(maximumForce, s.maximumForce)
        let (dx, dy) = transform.toDoc(s.x, s.y)
        let p = s.maximumForce > 0 ? NativeMapping.pencilPressure(fraction: s.force / s.maximumForce) : 0.5
        let tilt = NativeMapping.tilt(altitude: s.altitude, azimuth: s.azimuth)
        return InkPoint(x: r3(dx), y: r3(dy), pressure: (p * 10000).rounded() / 10000, tiltX: tilt.x, tiltY: tilt.y,
                        timeUs: Int64(((s.timestamp - (t0 ?? s.timestamp)) * 1_000_000).rounded()))
    }

    /// Appends real samples (the coalesced list for one touch event, in order).
    public func add(_ samples: [PencilSample], coalesced: Bool) {
        for s in samples {
            if let last = points.last, let lt = t0, Int64(((s.timestamp - lt) * 1_000_000).rounded()) < last.timeUs { continue }
            let pt = point(s)
            if let last = points.last, last.x == pt.x, last.y == pt.y, last.pressure == pt.pressure { continue }
            points.append(pt)
            var f = coalesced ? PointFlags.coalesced : 0
            if s.expectsForceUpdate, let idx = s.estimationIndex {
                pendingEstimates[idx] = points.count - 1
                f |= PointFlags.estimatedPressure
            }
            flags.append(f)
            if coalesced { coalescedCount += 1 }
        }
    }

    /// Predicted samples: display only, replaced on every event, never committed.
    public func setPredicted(_ samples: [PencilSample]) {
        predicted = samples.map(point)
    }

    /// `touchesEstimatedPropertiesUpdated`: refines force on an earlier sample.
    @discardableResult
    public func updateEstimated(index: Int, force: Double, maximumForce: Double, final: Bool) -> Bool {
        guard let i = pendingEstimates[index], i < points.count else { return false }
        points[i].pressure = maximumForce > 0 ? (NativeMapping.pencilPressure(fraction: force / maximumForce) * 10000).rounded() / 10000 : 0.5
        estimatedUpdates += 1
        if final { pendingEstimates[index] = nil }
        return true
    }

    /// Canonical stroke (`m00-strokes` document shape) for the web/Rust side.
    public func finish() -> [String: Any] {
        predicted = []
        return [
            "id": id,
            "source": "stylus",
            "input": ["api": "uikit-pencil", "device": "pencil", "coalescedSamples": coalescedCount,
                      "estimatedForceUpdates": estimatedUpdates, "pendingEstimates": pendingEstimates.count,
                      "maximumForce": maximumForce, "pressureCurve": NativeMapping.pencilCurveName] as [String: Any],
            "brush": ["type": brush.tool, "width": brush.width, "color": brush.color, "pressure": brush.pressure && brush.tool == "pen",
                      "opacity": brush.tool == "highlighter" ? brush.opacity : 1] as [String: Any],
            "points": zip(points, flags).map { p, f -> [String: Any] in
                var d: [String: Any] = ["x": p.x, "y": p.y, "pressure": p.pressure, "tiltX": p.tiltX, "tiltY": p.tiltY, "timeUs": p.timeUs]
                if f != 0 { d["flags"] = f }
                return d
            },
        ]
    }
}

/// Finished strokes stay on the native preview layer until the web renderer
/// acknowledges that it painted them (no flash, no double ink).
public final class HandoffQueue {
    public private(set) var pending: [(id: String, at: Double)] = []
    public init() {}
    public func finished(_ id: String, at: Double) { pending.append((id, at)) }
    /// Returns true if `id` was pending (it can now be removed from the preview).
    @discardableResult
    public func ack(_ id: String) -> Bool {
        guard let i = pending.firstIndex(where: { $0.id == id }) else { return false }
        pending.remove(at: i)
        return true
    }
    /// Strokes whose ack did not arrive in time (web view stalled): drop them
    /// from the preview so a stale overlay can never linger.
    public func expire(now: Double, timeout: Double = 2.0) -> [String] {
        let old = pending.filter { now - $0.at > timeout }.map(\.id)
        pending.removeAll { now - $0.at > timeout }
        return old
    }
}

/// CSS rect of the editor canvas -> frame in the web view's coordinate space.
/// WKWebView maps CSS px 1:1 to points at page zoom 1, offset by the scroll
/// view's content offset (which includes the safe-area content inset).
public enum OverlayFrame {
    public static func frame(cssX: Double, cssY: Double, cssW: Double, cssH: Double, zoom: Double, contentOffsetX: Double, contentOffsetY: Double)
        -> (x: Double, y: Double, width: Double, height: Double) {
        (cssX * zoom - contentOffsetX, cssY * zoom - contentOffsetY, cssW * zoom, cssH * zoom)
    }
}
