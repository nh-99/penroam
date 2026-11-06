// Pure conversions between the canonical stroke format and native pen models
// (UIKit/PencilKit). Foundation-only so they are unit-tested on Linux; native
// adapters only call these.
import Foundation

public enum NativeMapping {
    /// UITouch.maximumPossibleForce reported by Apple Pencil (measured on
    /// iPad13,4 / iPadOS 26.6.2: 4.1666…). PencilKit's PKStrokePoint.force
    /// uses the same scale; verified by the on-device interop test (forceRange).
    public static let pencilMaxForce = 25.0 / 6.0

    public static func pressure(fromForce f: Double, maxForce: Double = pencilMaxForce) -> Double {
        guard maxForce > 0 else { return 0.5 }
        return min(1, max(0, f / maxForce))
    }

    /// Name of the curve below, recorded in each stroke's `input` so raw force stays recoverable.
    public static let pencilCurveName = "exp-4.6"
    private static let pencilCurveK = 4.6

    /// Apple Pencil force as a fraction of its maximum -> canonical pressure. Writing uses only
    /// the bottom quarter of the range (measured on iPad13,4: median 0.11, 95th percentile 0.22),
    /// which the linear mapping turned into thin, nearly uniform ink. This puts ordinary writing
    /// mid-range, as the shared width formula expects. Same curve as `pencilPressure` in the web
    /// editor; it is strictly increasing, so nothing is clipped.
    public static func pencilPressure(fraction f: Double) -> Double {
        let x = min(1, max(0, f))
        return (1 - exp(-pencilCurveK * x)) / (1 - exp(-pencilCurveK))
    }

    /// Inverse of `pencilPressure`, for recovering the force fraction from a stored point.
    public static func pencilFraction(pressure p: Double) -> Double {
        let x = min(1, max(0, p))
        return -log(1 - x * (1 - exp(-pencilCurveK))) / pencilCurveK
    }

    public static func force(fromPressure p: Double, maxForce: Double = pencilMaxForce) -> Double {
        min(1, max(0, p)) * maxForce
    }

    /// Apple altitude (0 = parallel to screen, π/2 = perpendicular) and azimuth
    /// (radians, in view coordinates) -> W3C tiltX/tiltY in degrees.
    public static func tilt(altitude: Double, azimuth: Double) -> (x: Double, y: Double) {
        let t = tan(altitude)
        guard t.isFinite, t > 1e-9 else { return (0, 0) }
        return (atan(cos(azimuth) / t) * 180 / .pi, atan(sin(azimuth) / t) * 180 / .pi)
    }

    /// Inverse of `tilt`. A pen with zero tilt maps to altitude π/2, azimuth 0.
    public static func altitudeAzimuth(tiltX: Double, tiltY: Double) -> (altitude: Double, azimuth: Double) {
        let tx = tan(tiltX * .pi / 180), ty = tan(tiltY * .pi / 180)
        let r = hypot(tx, ty)
        if r < 1e-12 { return (.pi / 2, 0) }
        return (atan2(1, r), atan2(ty, tx))
    }

    /// Recovers (nominal width, pressure flag) from per-point PencilKit sizes
    /// written by `toPencil`. PencilKit has no per-stroke pressure flag, so a
    /// stroke whose sizes are constant while pressure varies is pressure-off.
    /// PencilKit quantizes force (≈2e-4), which perturbs a per-point inversion
    /// by ~1e-3, so the estimate averages over all points and snaps to 0.01.
    public static func invertWidth(sizes: [Double], pressures: [Double]) -> (Double, Bool) {
        guard !sizes.isEmpty else { return (3, true) }
        let snap = { (w: Double) in (w * 100).rounded() / 100 }
        let lo = sizes.min()!, hi = sizes.max()!
        let pMin = pressures.min() ?? 0.5, pMax = pressures.max() ?? 0.5
        if hi - lo <= 1e-3 * hi && pMax - pMin > 0.02 { return (snap(sizes.reduce(0, +) / Double(sizes.count)), false) }
        let factors = pressures.map { 0.4 + 1.2 * min(1, max(0, $0)) }
        // Points whose width hit the 0.5 floor carry no information about the base.
        let usable = zip(sizes, factors).filter { $0.0 > 0.5 + 1e-4 }
        let pairs = usable.isEmpty ? Array(zip(sizes, factors)) : usable
        let base = pairs.map(\.0).reduce(0, +) / pairs.map(\.1).reduce(0, +)
        return (snap(base), true)
    }

    /// Per-point rendered width used when importing canonical strokes into a
    /// native engine that needs explicit sizes (PencilKit PKStrokePoint.size).
    public static func importWidth(_ s: InkStroke, _ p: InkPoint) -> Double {
        Geometry.widthFor(s.brush.width, p.pressure, s.brush.pressure)
    }
}

/// Geometry comparison used by round-trip tests (automated and on-device).
public enum StrokeCompare {
    public struct Report: Codable {
        public var strokesA = 0, strokesB = 0
        public var orderPreserved = true
        public var pointCountMismatches = 0
        public var maxPositionError = 0.0
        public var meanPositionError = 0.0
        public var maxPressureError = 0.0
        public var maxTiltError = 0.0
        public var maxTimeErrorUs: Int64 = 0
        /// Strokes whose nominal width differs by more than 0.005 (half the 0.01 snap).
        public var widthMismatches = 0
        public var maxWidthError = 0.0
        public var pressureFlagMismatches = 0
    }

    /// Compares stroke lists pairwise by order (ids are allowed to differ).
    public static func compare(_ a: [InkStroke], _ b: [InkStroke]) -> Report {
        var r = Report()
        r.strokesA = a.count; r.strokesB = b.count
        var sum = 0.0, n = 0
        for (sa, sb) in zip(a, b) {
            if sa.points.count != sb.points.count { r.pointCountMismatches += 1 }
            let dw = abs(sa.brush.width - sb.brush.width)
            r.maxWidthError = max(r.maxWidthError, dw)
            if dw > 0.005 { r.widthMismatches += 1 }
            if sa.brush.pressure != sb.brush.pressure { r.pressureFlagMismatches += 1 }
            if let fa = sa.points.first, let fb = sb.points.first, hypot(fa.x - fb.x, fa.y - fb.y) > 2 { r.orderPreserved = false }
            for (pa, pb) in zip(sa.points, sb.points) {
                let d = hypot(pa.x - pb.x, pa.y - pb.y)
                r.maxPositionError = max(r.maxPositionError, d); sum += d; n += 1
                r.maxPressureError = max(r.maxPressureError, abs(pa.pressure - pb.pressure))
                r.maxTiltError = max(r.maxTiltError, abs(pa.tiltX - pb.tiltX), abs(pa.tiltY - pb.tiltY))
                r.maxTimeErrorUs = max(r.maxTimeErrorUs, abs(pa.timeUs - pb.timeUs))
            }
        }
        r.meanPositionError = n > 0 ? sum / Double(n) : 0
        return r
    }
}
