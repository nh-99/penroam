#if canImport(UIKit)
import Foundation
import Network

/// Minimal HTTP control server on 127.0.0.1:8765 so a Linux host can drive the
/// harness over the USB cable (usbmuxd tunnel; see tools/benchctl.py).
///   GET  /status            JSON status from the app
///   GET  /files             perf/strokes file list
///   GET  /file?name=<dir/n> file contents
///   POST /cmd?c=<command>   bench-native | bench-web | export | engine-native | engine-web | clear | reset
/// No touch input is synthesized; Pencil workloads still need a person.
final class ControlServer {
    static let port: UInt16 = 8765
    private var listener: NWListener?
    var handler: ((String, [String: String], Data) -> (Int, String, Data))?

    func start() {
        do {
            let params = NWParameters.tcp
            params.requiredLocalEndpoint = NWEndpoint.hostPort(host: "127.0.0.1", port: NWEndpoint.Port(rawValue: Self.port)!)
            let l = try NWListener(using: params)
            l.newConnectionHandler = { [weak self] c in self?.serve(c) }
            l.stateUpdateHandler = { NSLog("BENCHCTL|listener %@", "\($0)") }
            l.start(queue: .main)
            listener = l
        } catch {
            NSLog("BENCHCTL|listener failed %@", "\(error)")
        }
    }

    private func serve(_ c: NWConnection) {
        c.start(queue: .main)
        var buf = Data()
        func read() {
            c.receive(minimumIncompleteLength: 1, maximumLength: 65536) { [weak self] data, _, done, err in
                if let data { buf.append(data) }
                if let r = buf.range(of: Data("\r\n\r\n".utf8)) {
                    let head = String(decoding: buf[..<r.lowerBound], as: UTF8.self)
                    let len = head.split(separator: "\r\n").first { $0.lowercased().hasPrefix("content-length:") }
                        .flatMap { Int($0.split(separator: ":")[1].trimmingCharacters(in: .whitespaces)) } ?? 0
                    let body = buf[r.upperBound...]
                    if body.count >= len { self?.respond(c, head, Data(body.prefix(len))); return }
                    if done || err != nil { c.cancel() } else { read() }
                } else if done || err != nil { c.cancel() } else { read() }
            }
        }
        read()
    }

    private func respond(_ c: NWConnection, _ head: String, _ bodyIn: Data) {
        let first = head.split(separator: "\r\n").first.map(String.init) ?? ""
        let parts = first.split(separator: " ")
        let target = parts.count > 1 ? String(parts[1]) : "/"
        let comps = URLComponents(string: "http://x" + target)
        var q: [String: String] = [:]
        for i in comps?.queryItems ?? [] { q[i.name] = i.value ?? "" }
        let (code, type, body) = handler?(comps?.path ?? "/", q, bodyIn) ?? (404, "text/plain", Data("no handler".utf8))
        var resp = Data("HTTP/1.1 \(code) OK\r\nContent-Type: \(type)\r\nContent-Length: \(body.count)\r\nConnection: close\r\n\r\n".utf8)
        resp.append(body)
        c.send(content: resp, completion: .contentProcessed { _ in c.cancel() })
    }
}
#endif
