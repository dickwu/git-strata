// strata-pack-probe: P-pack only (gui.md §8.1), never in a release build.
// Runs from Contents/MacOS of the app, so Bundle.main is the app and
// SMAppService.agent finds Contents/Library/LaunchAgents/<plist>.
// Prints one JSON line; exit 0 = the call returned, 1 = it threw.
import Foundation
import ServiceManagement

let plistName = "io.github.dickwu.git-strata.daemon.plist"
let service = SMAppService.agent(plistName: plistName)

func statusName(_ s: SMAppService.Status) -> String {
    switch s {
    case .notRegistered: return "notRegistered"
    case .enabled: return "enabled"
    case .requiresApproval: return "requiresApproval"
    case .notFound: return "notFound"
    @unknown default: return "unknown(\(s.rawValue))"
    }
}

func emit(_ fields: [String: Any]) {
    let data = try! JSONSerialization.data(withJSONObject: fields, options: [.sortedKeys])
    print(String(data: data, encoding: .utf8)!)
}

let command = CommandLine.arguments.dropFirst().first ?? "status"
var out: [String: Any] = ["cmd": command, "bundle": Bundle.main.bundlePath, "plist": plistName]
do {
    switch command {
    case "register": try service.register()
    case "unregister": try service.unregister()
    case "status": break
    case "open-settings": SMAppService.openSystemSettingsLoginItems()
    default:
        FileHandle.standardError.write("usage: strata-pack-probe status|register|unregister|open-settings\n".data(using: .utf8)!)
        exit(2)
    }
    out["ok"] = true
    out["status"] = statusName(service.status)
    emit(out)
} catch {
    let e = error as NSError
    out["ok"] = false
    out["status"] = statusName(service.status)
    out["error_domain"] = e.domain
    out["error_code"] = e.code
    out["error"] = e.localizedDescription
    emit(out)
    exit(1)
}
