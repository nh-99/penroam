#if canImport(PencilKit) && canImport(UIKit)
import UIKit

/// Passive recognizer that records raw UITouch.force for Pencil touches on
/// the canvas, so the PKStrokePoint.force PencilKit stores can be calibrated
/// against it. It never recognizes, never cancels or delays touches, and runs
/// simultaneously with PencilKit's own drawing recognizer.
final class ForceProbe: UIGestureRecognizer, UIGestureRecognizerDelegate {
    private(set) var forces: [Double] = []
    private(set) var maxPossible: Double = 0

    override init(target: Any?, action: Selector?) {
        super.init(target: target, action: action)
        cancelsTouchesInView = false
        delaysTouchesBegan = false
        delaysTouchesEnded = false
        allowedTouchTypes = [NSNumber(value: UITouch.TouchType.pencil.rawValue)]
        delegate = self
    }

    /// Returns and clears the samples of the stroke that just ended.
    func take() -> (forces: [Double], maxPossible: Double) {
        defer { forces = [] }
        return (forces, maxPossible)
    }

    private func record(_ touches: Set<UITouch>, _ event: UIEvent) {
        for t in touches where t.type == .pencil {
            maxPossible = Double(t.maximumPossibleForce)
            for c in event.coalescedTouches(for: t) ?? [t] { forces.append(Double(c.force)) }
        }
    }

    override func touchesBegan(_ touches: Set<UITouch>, with event: UIEvent) {
        forces = []; record(touches, event)
    }
    override func touchesMoved(_ touches: Set<UITouch>, with event: UIEvent) { record(touches, event) }
    override func touchesEnded(_ touches: Set<UITouch>, with event: UIEvent) { record(touches, event); state = .failed }
    override func touchesCancelled(_ touches: Set<UITouch>, with event: UIEvent) { state = .failed }

    func gestureRecognizer(_ g: UIGestureRecognizer, shouldRecognizeSimultaneouslyWith other: UIGestureRecognizer) -> Bool { true }
}
#endif
