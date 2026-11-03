#if canImport(UIKit)
import InkCore
import SwiftUI
import UIKit

@main
struct BenchHostApp: App {
    var body: some SwiftUI.Scene {
        WindowGroup {
            RootRepresentable().ignoresSafeArea(.keyboard)
        }
    }
}

struct RootRepresentable: UIViewControllerRepresentable {
    func makeUIViewController(context: Context) -> RootViewController { RootViewController() }
    func updateUIViewController(_ vc: RootViewController, context: Context) {}
}

/// High-contrast, animation-free UI. Two engines:
///  - "Native UIKit": NativeInkView (Apple Pencil reference)
///  - "WKWebView (Tauri frontend)": WebHostView, which has its own toolbar
final class RootViewController: UIViewController {
    private let native = NativeInkView()
    private var webStore: WebHostView?
    private var web: WebHostView {
        if let w = webStore { return w }
        let w = WebHostView(); webStore = w; return w
    }
    private lazy var pencil = PencilHostView()
    enum Engine: String { case native = "native-uikit", pencil = "pencilkit", web = "wkwebview-tauri-frontend" }
    private var engine: Engine = .native
    private let toolbar = UIStackView()
    private let modeBar = UIStackView()
    private let status = UILabel()
    private let debug = UILabel()
    private let content = UIView()
    private var buttons: [String: UIButton] = [:]
    private var useWeb: Bool { engine == .web }
    private var widthIdx = 1
    private let widths = [1.5, 3.0, 6.0]
    private var timer: Timer?
    private let control = ControlServer()
    private var nativeBenchRunning = false

    override var prefersStatusBarHidden: Bool { true }
    override var prefersHomeIndicatorAutoHidden: Bool { true }
    override var preferredScreenEdgesDeferringSystemGestures: UIRectEdge { .all }

    override func viewDidLoad() {
        super.viewDidLoad()
        UIView.setAnimationsEnabled(false)
        view.backgroundColor = .white
        for sv in [modeBar, toolbar] { sv.axis = .horizontal; sv.spacing = 4; sv.alignment = .center }

        addButton(modeBar, "native", "Native UIKit") { [unowned self] in setEngine(.native) }
        addButton(modeBar, "pencil", "PencilKit (shared module)") { [unowned self] in setEngine(.pencil) }
        addButton(modeBar, "web", "WKWebView (Tauri frontend)") { [unowned self] in setEngine(.web) }

        addButton(toolbar, "pen", "Pen") { [unowned self] in native.tool = .pen; syncPencilTool(); refresh() }
        addButton(toolbar, "eraser", "Eraser") { [unowned self] in native.tool = .eraser; syncPencilTool(); refresh() }
        addButton(toolbar, "width", "W3.0") { [unowned self] in widthIdx = (widthIdx + 1) % 3; native.width = widths[widthIdx]; syncPencilTool(); refresh() }
        addButton(toolbar, "pressure", "Pressure ON") { [unowned self] in native.pressureEnabled.toggle(); refresh() }
        addButton(toolbar, "clear", "Clear") { [unowned self] in engine == .pencil ? pencil.backend.clear() : native.clear() }
        addButton(toolbar, "interop", "Interop test") { [unowned self] in runInterop() }
        addButton(toolbar, "pkexport", "Export strokes") { [unowned self] in exportPencilStrokes() }
        addButton(toolbar, "style", "Smoothed") { [unowned self] in native.polyline.toggle(); refresh() }
        addButton(toolbar, "predicted", "Predicted OFF") { [unowned self] in native.usePredicted.toggle(); refresh() }
        addButton(toolbar, "sample", "Sample") { [unowned self] in loadSample() }
        for n in [100, 1000, 5000, 10000] {
            addButton(toolbar, "scene\(n)", n >= 1000 ? "Scene \(n / 1000)k" : "Scene \(n)") { [unowned self] in loadScene(n) }
        }
        addButton(toolbar, "bench", "Bench D") { [unowned self] in runBench(completion: nil) }
        addButton(toolbar, "export", "Export perf") { [unowned self] in exportPerf(label: "native-uikit") }
        addButton(toolbar, "reset", "Reset perf") { [unowned self] in native.metrics.reset() }
        addButton(toolbar, "zoom", "1:1") { [unowned self] in native.setTransform(scale: 1, offset: .zero) }

        status.font = .boldSystemFont(ofSize: 13); status.numberOfLines = 2
        debug.font = .monospacedSystemFont(ofSize: 11, weight: .regular); debug.numberOfLines = 4

        let toolScroll = UIScrollView(); toolScroll.addSubview(toolbar); toolScroll.showsHorizontalScrollIndicator = false
        let stack = UIStackView(arrangedSubviews: [modeBar, toolScroll, status, debug, content])
        stack.axis = .vertical; stack.spacing = 2
        stack.translatesAutoresizingMaskIntoConstraints = false
        toolbar.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(stack)
        let g = view.safeAreaLayoutGuide
        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: g.leadingAnchor, constant: 4),
            stack.trailingAnchor.constraint(equalTo: g.trailingAnchor, constant: -4),
            stack.topAnchor.constraint(equalTo: g.topAnchor, constant: 4),
            stack.bottomAnchor.constraint(equalTo: view.bottomAnchor),
            toolScroll.heightAnchor.constraint(equalToConstant: 40),
            toolbar.leadingAnchor.constraint(equalTo: toolScroll.contentLayoutGuide.leadingAnchor),
            toolbar.trailingAnchor.constraint(equalTo: toolScroll.contentLayoutGuide.trailingAnchor),
            toolbar.topAnchor.constraint(equalTo: toolScroll.contentLayoutGuide.topAnchor),
            toolbar.bottomAnchor.constraint(equalTo: toolScroll.contentLayoutGuide.bottomAnchor),
            toolbar.heightAnchor.constraint(equalTo: toolScroll.frameLayoutGuide.heightAnchor),
        ])
        native.metrics.info["system"] = .string("\(Export.systemInfo())")
        native.metrics.timing("startup_to_first_frame_ms", Export.processAgeMs())
        setEngine(.native)
        timer = Timer.scheduledTimer(withTimeInterval: 0.25, repeats: true) { [weak self] _ in self?.updateDebug() }
        startAutorunIfRequested()
        control.handler = { [unowned self] path, q, body in handleControl(path, q, body) }
        control.start()
    }

    private func json(_ o: Any) -> Data { (try? JSONSerialization.data(withJSONObject: o, options: [.prettyPrinted, .sortedKeys])) ?? Data() }

    private func handleControl(_ path: String, _ q: [String: String], _ body: Data) -> (Int, String, Data) {
        switch path {
        case "/status":
            return (200, "application/json", json([
                "engine": engine.rawValue,
                "pencilStrokes": pencil.backend.strokeCount(),
                "webFrameRate": WebHostView.frameRateResult,
                "pencilCounters": pencil.backend.metrics.counters,
                "status": status.text ?? "", "debug": debug.text ?? "",
                "nativeBenchRunning": nativeBenchRunning, "webLastEvent": WebHostView.lastEvent,
                "strokes": native.strokes.count, "counters": native.metrics.counters,
                "system": Export.systemInfo(), "proc": Export.procStats(note: "app process"),
                "uptimeMs": Export.processAgeMs(),
            ]))
        case "/files":
            return (200, "application/json", json(["perf": Export.list(dir: "perf"), "strokes": Export.list(dir: "strokes")]))
        case "/file":
            let parts = (q["name"] ?? "").split(separator: "/", maxSplits: 1).map(String.init)
            guard parts.count == 2, let text = try? Export.load(dir: parts[0], name: parts[1]) else { return (404, "text/plain", Data("not found".utf8)) }
            return (200, "application/json", Data(text.utf8))
        case "/cmd":
            switch q["c"] ?? "" {
            case "bench-native":
                guard !nativeBenchRunning else { return (409, "text/plain", Data("busy".utf8)) }
                setEngine(.native); nativeBenchRunning = true
                runBench { [weak self] in self?.nativeBenchRunning = false }
            case "bench-web":
                setEngine(.web); WebHostView.lastEvent = "web-bench-running"; web.runAutobench()
            case "export": exportPerf(label: "native-uikit")
            case "engine-native": setEngine(.native)
            case "engine-web": setEngine(.web)
            case "engine-pencilkit": setEngine(.pencil)
            case "interop": runInterop()
            case "web120":
                // off | early | late — recreates the WKWebView with that variant
                UserDefaults.standard.set(q["mode"] ?? "early", forKey: "bench.web120mode")
                webStore?.removeFromSuperview(); webStore = nil
                setEngine(.web)
                return (200, "application/json", json(WebHostView.frameRateResult))
            case "pencil-export": exportPencilStrokes()
            case "pencil-metrics": exportPencilMetrics()
            case "pencil-import":
                // Body: canonical stroke document (e.g. edited on Linux) -> PencilKit
                do { let n = try pencil.importCanonical(body); setEngine(.pencil); return (200, "text/plain", Data("imported \(n)".utf8)) }
                catch { return (400, "text/plain", Data("\(error)".utf8)) }
            case "clear": engine == .pencil ? pencil.backend.clear() : native.clear()
            case "reset": native.metrics.reset()
            default: return (400, "text/plain", Data("unknown command".utf8))
            }
            return (200, "text/plain", Data("ok".utf8))
        default:
            return (404, "text/plain", Data("unknown path".utf8))
        }
    }

    /// One-shot automated sequence, enabled by building with a bundled
    /// `autorun.txt` (tools/install_autorun.sh). Runs once per build stamp:
    /// native Workload D, then the Tauri frontend's Workload D in WKWebView.
    private func startAutorunIfRequested() {
        guard let url = Bundle.module.url(forResource: "autorun", withExtension: "txt", subdirectory: "autorun"),
              let stamp = try? String(contentsOf: url, encoding: .utf8).trimmingCharacters(in: .whitespacesAndNewlines),
              UserDefaults.standard.string(forKey: "autorunDone") != stamp else { return }
        UserDefaults.standard.set(stamp, forKey: "autorunDone")
        NSLog("BENCHAUTORUN|start|%@", stamp)
        DispatchQueue.main.asyncAfter(deadline: .now() + 1.5) { [self] in
            nativeBenchRunning = true
            runBench { [self] in
                nativeBenchRunning = false
                NSLog("BENCHAUTORUN|native-done")
                WebHostView.autobench = true
                setEngine(.web)
            }
        }
    }

    private func addButton(_ sv: UIStackView, _ key: String, _ title: String, _ action: @escaping () -> Void) {
        var cfg = UIButton.Configuration.plain()
        cfg.title = title
        cfg.baseForegroundColor = .black
        cfg.contentInsets = NSDirectionalEdgeInsets(top: 5, leading: 8, bottom: 5, trailing: 8)
        cfg.background.strokeColor = .black; cfg.background.strokeWidth = 2; cfg.background.cornerRadius = 0
        let b = UIButton(configuration: cfg, primaryAction: UIAction { _ in action() })
        sv.addArrangedSubview(b)
        buttons[key] = b
    }

    private func setTitle(_ key: String, _ t: String, on: Bool = false) {
        guard let b = buttons[key] else { return }
        var c = b.configuration!
        c.title = t
        c.baseForegroundColor = on ? .white : .black
        c.background.backgroundColor = on ? .black : .white
        b.configuration = c
    }

    private func setEngine(_ e: Engine) {
        engine = e
        content.subviews.forEach { $0.removeFromSuperview() }
        let v: UIView = e == .web ? web : (e == .pencil ? pencil : native)
        v.frame = content.bounds
        v.autoresizingMask = [.flexibleWidth, .flexibleHeight]
        content.addSubview(v)
        toolbar.superview?.isHidden = e == .web
        debug.isHidden = e == .web
        if e == .pencil { syncPencilTool() }
        refresh()
    }

    private func syncPencilTool() {
        pencil.backend.setTool(native.tool == .eraser ? .eraser : .pen, width: native.width)
    }

    private func loadFixture(_ name: String) -> [InkStroke] {
        guard let url = Bundle.module.url(forResource: name, withExtension: "json", subdirectory: "fixtures"),
              let d = try? Data(contentsOf: url), let doc = try? JSONDecoder().decode(InkDocument.self, from: d) else { return [] }
        return doc.strokes
    }

    private func runInterop() {
        status.text = "interop test running…"
        let report = pencil.runInteropTest(fixtures: ["sample-strokes": loadFixture("sample-strokes"),
                                                      "scene-100": InkCore.Scene.generate(100)])
        save(report, label: "pencilkit-interop")
    }

    private func exportPencilStrokes() {
        do {
            let r = try pencil.exportBoth()
            status.text = "exported \(pencil.backend.strokeCount()) strokes → \(r.canonical) (+ native PKDrawing)"
        } catch { status.text = "export failed: \(error)" }
    }

    private func exportPencilMetrics() {
        save(["app": "ios-pencilkit", "exportedAt": ISO8601DateFormatter().string(from: Date()), "system": Export.systemInfo(),
              "proc": Export.procStats(note: "app process (PencilKit renders in-process)"), "metrics": pencil.backend.getMetrics()],
             label: "pencilkit")
    }

    private func refresh() {
        setTitle("native", "Native UIKit", on: engine == .native)
        setTitle("pencil", "PencilKit (shared module)", on: engine == .pencil)
        setTitle("web", "WKWebView (Tauri frontend)", on: useWeb)
        setTitle("pen", "Pen", on: native.tool == .pen)
        setTitle("eraser", "Eraser", on: native.tool == .eraser)
        setTitle("width", "W\(widths[widthIdx])")
        setTitle("pressure", native.pressureEnabled ? "Pressure ON" : "Pressure OFF", on: native.pressureEnabled)
        setTitle("style", native.polyline ? "Raw polyline" : "Smoothed")
        setTitle("predicted", native.usePredicted ? "Predicted ON" : "Predicted OFF", on: native.usePredicted)
        status.text = switch engine {
        case .web: "ENGINE: WKWebView running the Tauri frontend (proxy for the Tauri iPad build — not the Tauri app)"
        case .pencil: "ENGINE: PencilKit via shared PencilInk module (standalone UIKit/PencilKit reference) — fingers pan/zoom"
        case .native: "ENGINE: Native UIKit Apple Pencil reference (custom renderer)"
        }
    }

    private func updateDebug() {
        guard engine == .native else {
            if engine == .pencil {
                let c = pencil.backend.metrics.counters
                func f(_ k: String) -> String { guard let s = pencil.backend.metrics.get(k), let m = s["median"], let p = s["p95"] else { return "-" }; return String(format: "%.1f/%.1f", m, p) }
                debug.text = "pencilkit strokes \(pencil.backend.strokeCount()) · committed \(c["strokes_committed"] ?? 0) · erased \(c["strokes_erased"] ?? 0) · drawingDidChange \(c["drawing_did_change"] ?? 0)\nmed/p95 ms  toolEnd→commit \(f("tool_end_to_commit_ms"))  diff+convert \(f("diff_convert_ms"))  stroke dur \(f("stroke_duration_ms"))\ncaps: \(pencil.backend.getCapabilities()["observedForceRange"] ?? [])"
            }
            return
        }
        let m = native.metrics
        func f(_ k: String) -> String {
            guard let s = m.get(k), let med = s["median"], let p95 = s["p95"] else { return "-" }
            return String(format: "%.1f/%.1f", med, p95)
        }
        let last = m.strokes.last
        func lv(_ k: String) -> String { if case .number(let d)? = last?[k] { return String(format: "%.1f", d) }; return "-" }
        debug.text = "strokes \(native.strokes.count) · events \(m.counters["pointer_event_received"] ?? 0) · samples \(m.counters["input_samples"] ?? 0) · over-budget \(m.counters["frames_over_budget"] ?? 0) · est.force updates \(m.counters["estimated_force_updates"] ?? 0) · hover \(m.counters["hover_events"] ?? 0) · dbl-tap \(m.counters["pencil_double_tap"] ?? 0)\n" +
            "med/p95 ms  evt→handler \(f("event_to_handler_ms"))  evt→expected present \(f("event_to_expected_present_ms"))  draw \(f("preview_render_submitted_ms"))  frame \(f("frame_interval_ms"))  commit \(f("commit_cpu_ms"))\n" +
            (last != nil ? "last stroke n=\(lv("samples")) coalesced=\(lv("coalescedSamples")) dt med \(lv("medianDtMs")) max \(lv("maxDtMs")) gaps \(lv("timeGaps")) jumps \(lv("spatialJumps"))\n" : "") +
            "pencil: \(m.info["device_pencil"].map { "\($0)" } ?? "-")"
    }

    private func loadSample() {
        guard let url = Bundle.module.url(forResource: "sample-strokes", withExtension: "json", subdirectory: "fixtures"),
              let data = try? Data(contentsOf: url),
              let doc = try? JSONDecoder().decode(InkDocument.self, from: data) else { status.text = "sample missing"; return }
        let ms = native.setStrokes(doc.strokes)
        status.text = String(format: "sample: %d strokes, render %.0f ms", doc.strokes.count, ms)
    }

    private func loadScene(_ n: Int) {
        let t0 = CACurrentMediaTime()
        let s = InkCore.Scene.generate(n)
        let gen = (CACurrentMediaTime() - t0) * 1000
        let ms = native.setStrokes(s)
        native.metrics.timing("scene_\(n)_render_ms", ms)
        status.text = String(format: "scene %d: generate %.0f ms, render %.0f ms", n, gen, ms)
    }

    private func runBench(completion: (() -> Void)? = nil) {
        guard let url = Bundle.module.url(forResource: "sample-strokes", withExtension: "json", subdirectory: "fixtures"),
              let data = try? Data(contentsOf: url),
              let doc = try? JSONDecoder().decode(InkDocument.self, from: data),
              let curve = doc.strokes.first(where: { $0.id == "sample-curve" }) else { return }
        let replay = curve.points.map { InkPoint(x: $0.x + 200, y: $0.y + 300, pressure: $0.pressure, timeUs: $0.timeUs) }
        var results: [[String: Any]] = []
        var queue: [(Int, Int)] = []
        for n in [100, 1000, 5000, 10000] { for t in 0..<3 { queue.append((n, t)) } }
        func next() {
            guard !queue.isEmpty else {
                native.clear()
                let out: [String: Any] = ["app": "ios-native-uikit", "workload": "D-large-document", "exportedAt": ISO8601DateFormatter().string(from: Date()),
                                          "system": Export.systemInfo(), "viewport": ["width": native.bounds.width, "height": native.bounds.height, "dpr": native.window?.screen.scale ?? 2],
                                          "results": results, "metrics": native.metrics.toJSON()]
                save(out, label: "native-uikit-workload-d")
                completion?()
                return
            }
            let (n, trial) = queue.removeFirst()
            status.text = "bench: scene \(n) trial \(trial + 1)/3"
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.05) { [self] in
                let t0 = CACurrentMediaTime()
                let s = InkCore.Scene.generate(n)
                let gen = (CACurrentMediaTime() - t0) * 1000
                let render = native.setStrokes(s)
                let rebuild = native.rebuild()
                native.replay(replay) { frames in
                    let sorted = frames.sorted()
                    let ps = Export.procStats(note: "")
                    results.append(["strokes": n, "trial": trial, "generateMs": gen, "initialRenderMs": render, "rebuildMs": rebuild,
                                    "replayFrameMedianMs": sorted[sorted.count / 2], "replayFrameMaxMs": sorted.last ?? 0,
                                    "replayFrames": frames.count, "physFootprintBytes": ps["physFootprintBytes"] ?? 0])
                    next()
                }
            }
        }
        next()
    }

    private func exportPerf(label: String) {
        let out: [String: Any] = ["app": "ios-native-uikit", "exportedAt": ISO8601DateFormatter().string(from: Date()),
                                  "system": Export.systemInfo(), "proc": Export.procStats(note: "app process"), "metrics": native.metrics.toJSON()]
        save(out, label: label)
    }

    private func save(_ obj: [String: Any], label: String) {
        do {
            let data = try JSONSerialization.data(withJSONObject: obj, options: [.prettyPrinted, .sortedKeys])
            let p = try Export.save(dir: "perf", name: "perf-ios-\(label)-\(Export.stamp()).json", text: String(decoding: data, as: UTF8.self), log: true)
            status.text = "perf → \(p) (also streamed to device log)"
        } catch {
            status.text = "export failed: \(error)"
        }
    }
}
#endif
