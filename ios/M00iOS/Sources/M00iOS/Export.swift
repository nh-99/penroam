#if canImport(UIKit)
import Darwin
import Foundation
import UIKit

/// Files live in the app's Documents (visible in the Files app because of
/// UIFileSharingEnabled). Because the build machine is Linux, perf exports are
/// also written to the device log in tagged base64 chunks; collect them with
/// `ios/M00iOS/tools/collect_results.py` (uses idevicesyslog).
enum Export {
    static func stamp() -> String { String(Int(Date().timeIntervalSince1970 * 1000)) }

    static func dir(_ sub: String) throws -> URL {
        let d = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0].appendingPathComponent(sub)
        try FileManager.default.createDirectory(at: d, withIntermediateDirectories: true)
        return d
    }

    @discardableResult
    static func save(dir sub: String, name: String, text: String, log: Bool) throws -> String {
        let url = try dir(sub).appendingPathComponent(name)
        try text.write(to: url, atomically: true, encoding: .utf8)
        if log { logChunks(name: name, text: text) }
        return url.path
    }

    static func list(dir sub: String) -> [String] {
        let names = (try? FileManager.default.contentsOfDirectory(atPath: (try? dir(sub).path) ?? "")) ?? []
        return names.filter { $0.hasSuffix(".json") || $0.hasSuffix(".b64") }.sorted().reversed()
    }

    static func load(dir sub: String, name: String) throws -> String {
        guard !name.contains("/"), !name.contains("..") else { throw URLError(.badURL) }
        return try String(contentsOf: dir(sub).appendingPathComponent(name), encoding: .utf8)
    }

    /// M00X|<name>|<i>|<n>|<base64 chunk>
    static func logChunks(name: String, text: String) {
        let b64 = Data(text.utf8).base64EncodedString()
        let size = 700
        let n = (b64.count + size - 1) / size
        var i = 0
        var idx = b64.startIndex
        while idx < b64.endIndex {
            let end = b64.index(idx, offsetBy: size, limitedBy: b64.endIndex) ?? b64.endIndex
            NSLog("M00X|%@|%d|%d|%@", name, i, n, String(b64[idx..<end]))
            idx = end; i += 1
            if i % 50 == 0 { usleep(20_000) } // keep syslog from dropping lines
        }
    }

    static func machine() -> String {
        var u = utsname(); uname(&u)
        return withUnsafeBytes(of: &u.machine) { String(decoding: $0.prefix(while: { $0 != 0 }), as: UTF8.self) }
    }

    static func systemInfo(extra: [String: String] = [:]) -> [String: Any] {
        let d = UIDevice.current
        var o: [String: Any] = [
            "os": "ios", "osVersion": d.systemVersion, "model": d.model, "machine": machine(),
            "maxFPS": UIScreen.main.maximumFramesPerSecond, "screenScale": UIScreen.main.scale,
            "screen": ["w": UIScreen.main.bounds.width, "h": UIScreen.main.bounds.height],
            "thermalState": ProcessInfo.processInfo.thermalState.rawValue,
            "lowPowerMode": ProcessInfo.processInfo.isLowPowerModeEnabled,
        ]
        for (k, v) in extra { o[k] = v }
        return o
    }

    static func procStats(note: String) -> [String: Any] {
        var info = task_vm_info_data_t()
        var count = mach_msg_type_number_t(MemoryLayout<task_vm_info_data_t>.size / MemoryLayout<integer_t>.size)
        let kr = withUnsafeMutablePointer(to: &info) {
            $0.withMemoryRebound(to: integer_t.self, capacity: Int(count)) { task_info(mach_task_self_, task_flavor_t(TASK_VM_INFO), $0, &count) }
        }
        var ru = rusage(); getrusage(RUSAGE_SELF, &ru)
        let cpu = Double(ru.ru_utime.tv_sec + ru.ru_stime.tv_sec) + Double(ru.ru_utime.tv_usec + ru.ru_stime.tv_usec) / 1e6
        let foot = kr == KERN_SUCCESS ? Int(info.phys_footprint) : 0
        return ["available": kr == KERN_SUCCESS, "pid": Int(getpid()), "rssBytes": foot, "physFootprintBytes": foot,
                "cpuSeconds": cpu, "treeRssBytes": foot, "treeCpuSeconds": cpu, "processes": [], "note": note]
    }

    /// ms since process start (sysctl KERN_PROC start time).
    static func processAgeMs() -> Double {
        var kp = kinfo_proc()
        var size = MemoryLayout<kinfo_proc>.stride
        var mib: [Int32] = [CTL_KERN, KERN_PROC, KERN_PROC_PID, getpid()]
        guard sysctl(&mib, 4, &kp, &size, nil, 0) == 0 else { return -1 }
        let st = kp.kp_proc.p_un.__p_starttime
        let start = Double(st.tv_sec) + Double(st.tv_usec) / 1e6
        return (Date().timeIntervalSince1970 - start) * 1000
    }
}
#endif
