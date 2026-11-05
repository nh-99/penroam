// Sends a finished PDF to the iOS print sheet. Printer, copies and page range are chosen there,
// and the system cuts the document down to the chosen pages itself.
//
// Reachable only from the app's Rust side (see the plugin's `lib.rs`).
#if canImport(UIKit)
import SwiftRs
import Tauri
import UIKit
import WebKit

final class NativePrintPlugin: Plugin {
    private weak var webview: WKWebView?

    struct PrintArgs: Decodable {
        let path: String
        let title: String
    }

    override func load(webview: WKWebView) {
        self.webview = webview
    }

    @objc public func printPdf(_ invoke: Invoke) throws {
        let args = try invoke.parseArgs(PrintArgs.self)
        let file = URL(fileURLWithPath: args.path)
        DispatchQueue.main.async {
            guard UIPrintInteractionController.canPrint(file), let view = self.webview else {
                try? FileManager.default.removeItem(at: file)
                invoke.reject("Printing is not available on this device.")
                return
            }
            let info = UIPrintInfo.printInfo()
            info.jobName = args.title
            info.outputType = .general
            let controller = UIPrintInteractionController.shared
            controller.printInfo = info
            controller.printingItem = file
            let finished: UIPrintInteractionController.CompletionHandler = { _, completed, error in
                try? FileManager.default.removeItem(at: file)
                if let error {
                    invoke.reject(error.localizedDescription)
                } else {
                    invoke.resolve(["status": completed ? "sent" : "cancelled"])
                }
            }
            // On iPad the sheet is a popover and needs an anchor; the top right holds the
            // Export menu. iPhone ignores it and uses a full sheet.
            let anchor = CGRect(x: view.bounds.maxX - 60, y: view.safeAreaInsets.top, width: 44, height: 44)
            controller.present(from: anchor, in: view, animated: true, completionHandler: finished)
        }
    }
}

@_cdecl("init_plugin_native_print")
func initPlugin() -> Plugin {
    return NativePrintPlugin()
}
#endif
