import AppKit
import GhosttyKit

// `ghostdrop ctl <command>` talks to the running instance and exits.
let args = CommandLine.arguments
if args.count >= 2 && args[1] == "ctl" {
    exit(Control.send(Array(args.dropFirst(2))))
}

// Don't let a parent terminal's environment leak into our shells.
for v in ["GHOSTTY_RESOURCES_DIR", "GHOSTTY_BIN_DIR", "GHOSTTY_SHELL_FEATURES", "GHOSTTY_QUICK_TERMINAL",
          "ITERM_SESSION_ID", "ITERM_PROFILE", "LC_TERMINAL", "LC_TERMINAL_VERSION", "TERM_SESSION_ID",
          "COLORFGBG", "TERMINFO"] {
    unsetenv(v)
}

// Initialize libghostty's global state. Only argv[0]: our arguments aren't Ghostty CLI actions.
var argv0: [UnsafeMutablePointer<CChar>?] = [strdup(args[0]), nil]
if ghostty_init(1, &argv0) != GHOSTTY_SUCCESS {
    FileHandle.standardError.write("ghostdrop: ghostty_init failed\n".data(using: .utf8)!)
    exit(1)
}

// `ghostdrop check`: validate the config and exit.
if args.count >= 2 && args[1] == "check" {
    let settings = Settings.load()
    _ = GhosttyRuntime.makeConfig(settings)
    print("config: \(Settings.path)")
    print("hotkey: \(settings.hotkey?.description ?? "none")")
    print("profiles: " + settings.profileOrder.map { name -> String in
        let p = settings.profiles[name]!
        return "\(name)\(name == settings.defaultProfileName ? "*" : "") → \(p.effectiveCommand ?? "login shell")"
    }.joined(separator: ", "))
    print("bindings: " + settings.bindings.map { "\($0.0)=\($0.1)" }.joined(separator: ", "))
    if settings.errors.isEmpty {
        print("OK")
        exit(0)
    }
    settings.errors.forEach { print("error: \($0)") }
    exit(1)
}

let app = NSApplication.shared
let delegate = AppDelegate()
app.delegate = delegate
app.setActivationPolicy(.accessory)
app.run()
