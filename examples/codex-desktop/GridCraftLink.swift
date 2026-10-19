// A native URL receiver for our own application. No UI scripting or Desktop automation.
import AppKit

final class LinkDelegate: NSObject, NSApplicationDelegate {
    private var tasks: [UUID: Process] = [:]

    func application(_ application: NSApplication, open urls: [URL]) {
        for url in urls {
            guard url.scheme == "gridcraft", url.host == "open",
                  let helper = Bundle.main.object(forInfoDictionaryKey: "GridCraftLinkHelper") as? String else {
                continue
            }
            let task = Process()
            let identity = UUID()
            task.executableURL = URL(fileURLWithPath: "/usr/bin/python3")
            task.arguments = [helper, "open", url.absoluteString]
            if let state = Bundle.main.object(forInfoDictionaryKey: "GridCraftStateDirectory") as? String {
                var environment = ProcessInfo.processInfo.environment
                environment["GRIDCRAFT_CODEX_STATE_DIR"] = state
                task.environment = environment
            }
            let output = Pipe()
            task.standardOutput = output
            task.standardError = output
            task.terminationHandler = { completed in
                let text = String(data: output.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8) ?? ""
                DispatchQueue.main.async {
                    self.tasks.removeValue(forKey: identity)
                    if completed.terminationStatus != 0 {
                        let alert = NSAlert()
                        alert.messageText = "Не удалось открыть ссылку GridCraft"
                        alert.informativeText = String(text.prefix(1500))
                        alert.runModal()
                    }
                }
            }
            do {
                tasks[identity] = task
                try task.run()
            } catch {
                tasks.removeValue(forKey: identity)
                NSLog("GridCraft link: %@", error.localizedDescription)
            }
        }
    }
}

let application = NSApplication.shared
let delegate = LinkDelegate()
application.setActivationPolicy(.accessory)
application.delegate = delegate
application.run()
