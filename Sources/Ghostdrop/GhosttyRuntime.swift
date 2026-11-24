import AppKit
import Carbon.HIToolbox
import GhosttyKit
import UniformTypeIdentifiers
import UserNotifications

/// Receives the libghostty actions that need app/window level handling.
protocol GhosttyRuntimeDelegate: AnyObject {
    func runtime(_ rt: GhosttyRuntime, perform action: RuntimeAction, from surface: SurfaceView?) -> Bool
    func runtimeConfigDidChange(_ rt: GhosttyRuntime)
}

/// Swift-side, already-copied representation of the actions we handle. All C
/// pointers are converted before leaving the callback.
enum RuntimeAction {
    case newTab
    case closeTab(ghostty_action_close_tab_mode_e)
    case closeWindow
    case gotoTab(Int32)
    case moveTab(Int)
    case toggle
    case toggleFullscreen
    case resetWindowSize
    case quit
    case reloadConfig(soft: Bool)
    case openConfig
    case promptTitle
    case floatWindow(ghostty_action_float_window_e)
    case startSearch(String)
    case endSearch
    case searchTotal(Int)
    case searchSelected(Int)
    case copyTitle
}

final class GhosttyRuntime {
    private(set) var app: ghostty_app_t?
    private(set) var config: ghostty_config_t?
    weak var delegate: GhosttyRuntimeDelegate?
    private var tickPending = false
    private let tickLock = NSLock()

    init?(settings: Settings) {
        guard let cfg = Self.makeConfig(settings) else { return nil }
        config = cfg

        var rt = ghostty_runtime_config_s(
            userdata: Unmanaged.passUnretained(self).toOpaque(),
            supports_selection_clipboard: true,
            wakeup_cb: { ud in GhosttyRuntime.wakeup(ud) },
            action_cb: { app, target, action in GhosttyRuntime.action(app, target: target, action: action) },
            read_clipboard_cb: { ud, loc, state, mimes, mimesLen, list in
                GhosttyRuntime.readClipboard(ud, location: loc, state: state, mimes: mimes, mimesLen: mimesLen, list: list)
            },
            confirm_read_clipboard_cb: { ud, confirm, state, request in
                GhosttyRuntime.confirmReadClipboard(ud, confirm: confirm, state: state, request: request)
            },
            write_clipboard_cb: { ud, loc, content, len, confirm in
                GhosttyRuntime.writeClipboard(ud, location: loc, content: content, len: len, confirm: confirm)
            },
            close_surface_cb: { ud, alive in GhosttyRuntime.closeSurface(ud, processAlive: alive) }
        )
        guard let app = ghostty_app_new(&rt, cfg) else { return nil }
        self.app = app
        ghostty_app_set_focus(app, NSApp.isActive)

        let nc = NotificationCenter.default
        nc.addObserver(forName: NSApplication.didBecomeActiveNotification, object: nil, queue: .main) { [weak self] _ in
            if let app = self?.app { ghostty_app_set_focus(app, true) }
        }
        nc.addObserver(forName: NSApplication.didResignActiveNotification, object: nil, queue: .main) { [weak self] _ in
            if let app = self?.app { ghostty_app_set_focus(app, false) }
        }
        nc.addObserver(forName: NSTextInputContext.keyboardSelectionDidChangeNotification, object: nil, queue: .main) { [weak self] _ in
            if let app = self?.app { ghostty_app_keyboard_changed(app) }
        }
    }

    // MARK: Config

    /// Layering: Ghostdrop defaults < your Ghostty config < Ghostdrop config.
    static func makeConfig(_ settings: Settings) -> ghostty_config_t? {
        guard let cfg = ghostty_config_new() else { return nil }
        let dir = (NSTemporaryDirectory() as NSString).appendingPathComponent("ghostdrop-\(getuid())")
        try? FileManager.default.createDirectory(atPath: dir, withIntermediateDirectories: true)

        let defaults = (dir as NSString).appendingPathComponent("defaults.conf")
        try? Self.defaultGhosttyConfig.write(toFile: defaults, atomically: true, encoding: .utf8)
        ghostty_config_load_file(cfg, defaults)

        // Only when the user actually has a Ghostty config: otherwise libghostty
        // would create a Ghostty template file on Macs that don't use Ghostty.
        if settings.inheritGhosttyConfig && Self.hasGhosttyConfig {
            ghostty_config_load_default_files(cfg)
        }

        if !settings.ghosttyLines.isEmpty {
            let overrides = (dir as NSString).appendingPathComponent("overrides.conf")
            let body = settings.ghosttyLines.joined(separator: "\n") + "\n"
            try? body.write(toFile: overrides, atomically: true, encoding: .utf8)
            ghostty_config_load_file(cfg, overrides)
        }
        ghostty_config_load_recursive_files(cfg)
        ghostty_config_finalize(cfg)

        let n = ghostty_config_diagnostics_count(cfg)
        for i in 0..<n {
            let d = ghostty_config_get_diagnostic(cfg, i)
            if let m = d.message { settings.errors.append("ghostty: " + String(cString: m)) }
        }
        return cfg
    }

    static var hasGhosttyConfig: Bool {
        let home = NSHomeDirectory() as NSString
        let xdg = ProcessInfo.processInfo.environment["XDG_CONFIG_HOME"].flatMap { $0.isEmpty ? nil : $0 }
            ?? home.appendingPathComponent(".config")
        let support = home.appendingPathComponent("Library/Application Support/com.mitchellh.ghostty")
        let candidates = [
            (xdg as NSString).appendingPathComponent("ghostty/config"),
            (xdg as NSString).appendingPathComponent("ghostty/config.ghostty"),
            (support as NSString).appendingPathComponent("config"),
            (support as NSString).appendingPathComponent("config.ghostty"),
        ]
        return candidates.contains { FileManager.default.fileExists(atPath: $0) }
    }

    /// Ghostdrop's opinionated defaults, overridable by any config file.
    static let defaultGhosttyConfig = """
    quick-terminal-position = top
    quick-terminal-size = 50%
    quick-terminal-screen = mouse
    quick-terminal-animation-duration = 0
    quick-terminal-autohide = true
    """

    func reload(settings: Settings) {
        guard let app, let cfg = Self.makeConfig(settings) else { return }
        ghostty_app_update_config(app, cfg)
        let old = config
        config = cfg
        if let old { ghostty_config_free(old) }
        delegate?.runtimeConfigDidChange(self)
    }

    func string(_ key: String) -> String? {
        guard let config else { return nil }
        var v: UnsafePointer<Int8>?
        guard ghostty_config_get(config, &v, key, UInt(key.utf8.count)), let v else { return nil }
        return String(cString: v)
    }

    func double(_ key: String, _ def: Double) -> Double {
        guard let config else { return def }
        var v = def
        _ = ghostty_config_get(config, &v, key, UInt(key.utf8.count))
        return v
    }

    func bool(_ key: String, _ def: Bool) -> Bool {
        guard let config else { return def }
        var v = def
        _ = ghostty_config_get(config, &v, key, UInt(key.utf8.count))
        return v
    }

    func uint(_ key: String) -> UInt32 {
        guard let config else { return 0 }
        var v: CUnsignedInt = 0
        _ = ghostty_config_get(config, &v, key, UInt(key.utf8.count))
        return v
    }

    func color(_ key: String) -> NSColor? {
        guard let config else { return nil }
        var c = ghostty_config_color_s()
        guard ghostty_config_get(config, &c, key, UInt(key.utf8.count)) else { return nil }
        return NSColor(srgbRed: CGFloat(c.r) / 255, green: CGFloat(c.g) / 255, blue: CGFloat(c.b) / 255, alpha: 1)
    }

    var quickTerminalSize: ghostty_config_quick_terminal_size_s {
        var v = ghostty_config_quick_terminal_size_s()
        guard let config else { return v }
        let key = "quick-terminal-size"
        _ = ghostty_config_get(config, &v, key, UInt(key.utf8.count))
        return v
    }

    // MARK: Tick

    private static func wakeup(_ ud: UnsafeMutableRawPointer?) {
        guard let ud else { return }
        let rt = Unmanaged<GhosttyRuntime>.fromOpaque(ud).takeUnretainedValue()
        // Coalesce wakeups: one pending tick on the main queue is enough.
        rt.tickLock.lock()
        let schedule = !rt.tickPending
        rt.tickPending = true
        rt.tickLock.unlock()
        guard schedule else { return }
        DispatchQueue.main.async {
            rt.tickLock.lock()
            rt.tickPending = false
            rt.tickLock.unlock()
            if let app = rt.app { ghostty_app_tick(app) }
        }
    }

    // MARK: Helpers

    static func runtime(of app: ghostty_app_t?) -> GhosttyRuntime? {
        guard let app, let ud = ghostty_app_userdata(app) else { return nil }
        return Unmanaged<GhosttyRuntime>.fromOpaque(ud).takeUnretainedValue()
    }

    static func view(of surface: ghostty_surface_t?) -> SurfaceView? {
        guard let surface, let ud = ghostty_surface_userdata(surface) else { return nil }
        return Unmanaged<SurfaceView>.fromOpaque(ud).takeUnretainedValue()
    }

    static func view(fromUserdata ud: UnsafeMutableRawPointer?) -> SurfaceView? {
        guard let ud else { return nil }
        return Unmanaged<SurfaceView>.fromOpaque(ud).takeUnretainedValue()
    }

    /// Always defer to the next main-loop turn: libghostty invokes callbacks
    /// synchronously while holding non-recursive locks (e.g. renderer_state.mutex
    /// in mouseButtonCallback -> open_url) or iterating config slices, so running
    /// AppKit/sheet/focus work inline can re-enter libghostty and deadlock.
    private static func onMain(_ body: @escaping () -> Void) {
        DispatchQueue.main.async(execute: body)
    }

    // MARK: Actions

    private static func action(_ app: ghostty_app_t?, target: ghostty_target_s, action: ghostty_action_s) -> Bool {
        guard let rt = runtime(of: app) else { return false }
        let view: SurfaceView? = target.tag == GHOSTTY_TARGET_SURFACE ? Self.view(of: target.target.surface) : nil

        // Helper: forward to the delegate on the next main-loop turn.
        func forward(_ a: RuntimeAction) -> Bool {
            guard rt.delegate != nil else { return false }
            onMain { _ = rt.delegate?.runtime(rt, perform: a, from: view) }
            return true
        }

        switch action.tag {
        case GHOSTTY_ACTION_NEW_TAB, GHOSTTY_ACTION_NEW_WINDOW:
            return forward(.newTab)
        case GHOSTTY_ACTION_CLOSE_TAB:
            return forward(.closeTab(action.action.close_tab_mode))
        case GHOSTTY_ACTION_CLOSE_WINDOW, GHOSTTY_ACTION_CLOSE_ALL_WINDOWS:
            return forward(.closeWindow)
        case GHOSTTY_ACTION_GOTO_TAB:
            return forward(.gotoTab(action.action.goto_tab.rawValue))
        case GHOSTTY_ACTION_MOVE_TAB:
            return forward(.moveTab(Int(action.action.move_tab.amount)))
        case GHOSTTY_ACTION_TOGGLE_QUICK_TERMINAL, GHOSTTY_ACTION_TOGGLE_VISIBILITY:
            return forward(.toggle)
        case GHOSTTY_ACTION_TOGGLE_FULLSCREEN, GHOSTTY_ACTION_TOGGLE_MAXIMIZE:
            return forward(.toggleFullscreen)
        case GHOSTTY_ACTION_RESET_WINDOW_SIZE:
            return forward(.resetWindowSize)
        case GHOSTTY_ACTION_QUIT:
            return forward(.quit)
        case GHOSTTY_ACTION_RELOAD_CONFIG:
            return forward(.reloadConfig(soft: action.action.reload_config.soft))
        case GHOSTTY_ACTION_OPEN_CONFIG:
            return forward(.openConfig)
        case GHOSTTY_ACTION_PROMPT_TITLE:
            return forward(.promptTitle)
        case GHOSTTY_ACTION_FLOAT_WINDOW:
            return forward(.floatWindow(action.action.float_window))
        case GHOSTTY_ACTION_COPY_TITLE_TO_CLIPBOARD:
            return forward(.copyTitle)
        case GHOSTTY_ACTION_START_SEARCH:
            let needle = action.action.start_search.needle.map { String(cString: $0) } ?? ""
            return forward(.startSearch(needle))
        case GHOSTTY_ACTION_END_SEARCH:
            return forward(.endSearch)
        case GHOSTTY_ACTION_SEARCH_TOTAL:
            return forward(.searchTotal(Int(action.action.search_total.total)))
        case GHOSTTY_ACTION_SEARCH_SELECTED:
            return forward(.searchSelected(Int(action.action.search_selected.selected)))

        case GHOSTTY_ACTION_SET_TITLE, GHOSTTY_ACTION_SET_TAB_TITLE:
            let p = action.tag == GHOSTTY_ACTION_SET_TITLE ? action.action.set_title.title : action.action.set_tab_title.title
            let title = p.map { String(cString: $0) } ?? ""
            onMain { view?.setTitleFromTerminal(title) }
            return true

        case GHOSTTY_ACTION_PWD:
            let pwd = action.action.pwd.pwd.map { String(cString: $0) }
            onMain { view?.pwd = pwd }
            return true

        case GHOSTTY_ACTION_MOUSE_SHAPE:
            let shape = action.action.mouse_shape
            onMain { view?.setCursorShape(shape) }
            return true

        case GHOSTTY_ACTION_MOUSE_VISIBILITY:
            let visible = action.action.mouse_visibility == GHOSTTY_MOUSE_VISIBLE
            onMain { NSCursor.setHiddenUntilMouseMoves(!visible) }
            return true

        case GHOSTTY_ACTION_MOUSE_OVER_LINK:
            let v = action.action.mouse_over_link
            let url: String? = (v.len > 0 && v.url != nil)
                ? String(decoding: UnsafeRawBufferPointer(start: v.url, count: v.len), as: UTF8.self) : nil
            onMain { view?.hoverURL = url }
            return true

        case GHOSTTY_ACTION_CELL_SIZE:
            let s = action.action.cell_size
            onMain { view?.cellSize = NSSize(width: CGFloat(s.width), height: CGFloat(s.height)) }
            return true

        case GHOSTTY_ACTION_RING_BELL:
            onMain {
                guard view == nil || view?.surface != nil else { return }
                if rt.uint("bell-features") & 0b1 != 0 { NSSound.beep() }
                view?.ringBell()
            }
            return true

        case GHOSTTY_ACTION_OPEN_URL:
            let v = action.action.open_url
            guard let p = v.url else { return false }
            let s = String(decoding: UnsafeRawBufferPointer(start: p, count: Int(v.len)), as: UTF8.self)
            let kind = v.kind
            onMain { URLOpener.open(s, kind: kind, in: view?.window) }
            return true

        case GHOSTTY_ACTION_DESKTOP_NOTIFICATION:
            let n = action.action.desktop_notification
            let title = n.title.map { String(cString: $0) } ?? ""
            let body = n.body.map { String(cString: $0) } ?? ""
            onMain { Notifier.post(title: title, body: body) }
            return true

        case GHOSTTY_ACTION_COLOR_CHANGE:
            let c = action.action.color_change
            if c.kind == GHOSTTY_ACTION_COLOR_KIND_BACKGROUND {
                let color = NSColor(srgbRed: CGFloat(c.r) / 255, green: CGFloat(c.g) / 255, blue: CGFloat(c.b) / 255, alpha: 1)
                onMain { view?.backgroundColorOverride = color }
            }
            return true

        case GHOSTTY_ACTION_CONFIG_CHANGE:
            // App-level config changes are driven by us (reload); surface-level
            // ones don't affect chrome.
            return true

        case GHOSTTY_ACTION_SECURE_INPUT:
            let mode = action.action.secure_input
            onMain {
                if let view {
                    // ON/OFF come from password-prompt detection (opt-out via
                    // macos-auto-secure-input); TOGGLE is the user's keybinding.
                    guard mode == GHOSTTY_SECURE_INPUT_TOGGLE || rt.bool("macos-auto-secure-input", true) else { return }
                    view.setPasswordInput(mode)
                } else {
                    SecureInput.shared.setGlobal(mode)
                }
            }
            return true

        case GHOSTTY_ACTION_RENDERER_HEALTH:
            if action.action.renderer_health == GHOSTTY_RENDERER_HEALTH_UNHEALTHY {
                NSLog("ghostdrop: renderer reported unhealthy")
            }
            return true

        case GHOSTTY_ACTION_SHOW_CHILD_EXITED:
            // Let the core render its own "process exited" message.
            return false

        case GHOSTTY_ACTION_RENDER, GHOSTTY_ACTION_INITIAL_SIZE, GHOSTTY_ACTION_SIZE_LIMIT,
             GHOSTTY_ACTION_SCROLLBAR, GHOSTTY_ACTION_KEY_SEQUENCE, GHOSTTY_ACTION_KEY_TABLE,
             GHOSTTY_ACTION_SELECTION_CHANGED, GHOSTTY_ACTION_QUIT_TIMER, GHOSTTY_ACTION_READONLY,
             GHOSTTY_ACTION_PROGRESS_REPORT, GHOSTTY_ACTION_COMMAND_FINISHED:
            return true

        default:
            // Splits, inspector, undo, updates, etc. aren't supported.
            return false
        }
    }

    // MARK: Close

    private static func closeSurface(_ ud: UnsafeMutableRawPointer?, processAlive: Bool) {
        guard let view = view(fromUserdata: ud) else { return }
        onMain { view.owner?.surfaceRequestedClose(view, needsConfirm: processAlive) }
    }

    // MARK: Clipboard

    private static func readClipboard(
        _ ud: UnsafeMutableRawPointer?,
        location: ghostty_clipboard_e,
        state: UnsafeMutableRawPointer?,
        mimes: UnsafePointer<UnsafePointer<CChar>?>?,
        mimesLen: Int,
        list: Bool
    ) -> ghostty_clipboard_read_result_e {
        guard let view = view(fromUserdata: ud), view.surface != nil else {
            return GHOSTTY_CLIPBOARD_READ_UNSUPPORTED
        }
        guard let pb = Clipboard.pasteboard(location) else { return GHOSTTY_CLIPBOARD_READ_UNSUPPORTED }

        var contents: [Clipboard.Content] = []
        var seen = Set<String>()
        if let mimes {
            for i in 0..<mimesLen {
                guard let p = mimes[i] else { continue }
                let mime = String(cString: p)
                guard seen.insert(mime).inserted, let data = Clipboard.data(pb, mime: mime) else { continue }
                contents.append(.init(mime: mime, data: data))
            }
        }
        let available = list ? Clipboard.availableMimes(pb) : []
        if contents.isEmpty && !list { return GHOSTTY_CLIPBOARD_READ_UNAVAILABLE }
        onMain {
            guard let surface = view.surface else { return }
            Clipboard.complete(surface, contents: contents, available: available, state: state)
        }
        return GHOSTTY_CLIPBOARD_READ_STARTED
    }

    private static func confirmReadClipboard(
        _ ud: UnsafeMutableRawPointer?,
        confirm: UnsafePointer<ghostty_clipboard_confirm_s>?,
        state: UnsafeMutableRawPointer?,
        request: ghostty_clipboard_request_e
    ) {
        guard let view = view(fromUserdata: ud), view.surface != nil else { return }
        guard let confirm else {
            onMain {
                if let surface = view.surface { ghostty_surface_deny_clipboard_request(surface, state) }
            }
            return
        }
        let c = confirm.pointee
        var reps: [Clipboard.Content] = []
        if let contents = c.contents {
            for i in 0..<c.contents_len {
                let item = contents[i]
                guard let mime = item.mime else { continue }
                let data = (item.len > 0 && item.data != nil) ? Data(bytes: item.data, count: item.len) : Data()
                reps.append(.init(mime: String(cString: mime), data: data))
            }
        }
        var avail: [String] = []
        if let a = c.available {
            for i in 0..<c.available_len { if let p = a[i] { avail.append(String(cString: p)) } }
        }
        let text = reps.first(where: { $0.mime == "text/plain" }).flatMap { String(data: $0.data, encoding: .utf8) }
            ?? reps.map { "\($0.mime) (\($0.data.count) bytes)" }.joined(separator: "\n")
        let canRemember = c.can_remember

        let (title, info): (String, String) = {
            switch request {
            case GHOSTTY_CLIPBOARD_REQUEST_PASTE:
                return ("Potentially unsafe paste", "The text you're pasting contains newlines or control sequences that could run commands.")
            case GHOSTTY_CLIPBOARD_REQUEST_OSC_52_READ, GHOSTTY_CLIPBOARD_REQUEST_KITTY_READ, GHOSTTY_CLIPBOARD_REQUEST_LIST:
                return ("Allow clipboard read?", "A program in this terminal wants to read your clipboard.")
            default:
                return ("Allow clipboard access?", "A program in this terminal is requesting clipboard access.")
            }
        }()

        onMain {
            guard view.surface != nil else { return }
            Alerts.confirm(
                in: view.window, title: title, info: info, preview: text,
                ok: request == GHOSTTY_CLIPBOARD_REQUEST_PASTE ? "Paste" : "Allow",
                remember: canRemember
            ) { allowed, remember in
                guard let surface = view.surface else { return }
                if allowed {
                    Clipboard.complete(surface, contents: reps, available: avail, state: state, confirmed: true, remember: remember)
                } else {
                    ghostty_surface_deny_clipboard_request(surface, state)
                }
            }
        }
    }

    private static func writeClipboard(
        _ ud: UnsafeMutableRawPointer?,
        location: ghostty_clipboard_e,
        content: UnsafePointer<ghostty_clipboard_content_s>?,
        len: Int,
        confirm: Bool
    ) {
        guard let pb = Clipboard.pasteboard(location), let content, len > 0 else { return }
        var items: [Clipboard.Content] = []
        for i in 0..<len {
            let c = content[i]
            guard let mime = c.mime, let data = c.data else { continue }
            items.append(.init(mime: String(cString: mime), data: Data(bytes: data, count: c.len)))
        }
        guard !items.isEmpty else { return }
        let view = Self.view(fromUserdata: ud)

        if !confirm {
            onMain { Clipboard.write(pb, items) }
            return
        }
        guard let text = items.first(where: { $0.mime == "text/plain" }).flatMap({ String(data: $0.data, encoding: .utf8) }) else { return }
        onMain {
            guard view == nil || view?.surface != nil else { return }
            Alerts.confirm(
                in: view?.window, title: "Allow clipboard write?",
                info: "A program in this terminal wants to set your clipboard to:",
                preview: text, ok: "Allow", remember: false
            ) { allowed, _ in
                guard allowed else { return }
                pb.declareTypes([.string], owner: nil)
                pb.setString(text, forType: .string)
            }
        }
    }
}

// MARK: - Clipboard helpers

enum Clipboard {
    struct Content {
        let mime: String
        let data: Data
    }

    static let selection = NSPasteboard(name: .init("com.ghostdrop.selection"))

    static func pasteboard(_ loc: ghostty_clipboard_e) -> NSPasteboard? {
        switch loc {
        case GHOSTTY_CLIPBOARD_STANDARD: return .general
        case GHOSTTY_CLIPBOARD_SELECTION: return selection
        default: return nil
        }
    }

    static func type(forMime mime: String) -> NSPasteboard.PasteboardType? {
        if mime == "text/plain" { return .string }
        if let ut = UTType(mimeType: mime) { return .init(ut.identifier) }
        return .init(mime)
    }

    private static func fileURLs(_ pb: NSPasteboard) -> [URL] {
        (pb.pasteboardItems ?? []).compactMap { item in
            guard let plist = item.propertyList(forType: .fileURL),
                  let url = NSURL(pasteboardPropertyList: plist, ofType: .fileURL) as URL?,
                  url.isFileURL else { return nil }
            return url
        }
    }

    static func string(_ pb: NSPasteboard) -> String? {
        let strings = (pb.pasteboardItems ?? []).compactMap { item -> String? in
            if let plist = item.propertyList(forType: .fileURL),
               let url = NSURL(pasteboardPropertyList: plist, ofType: .fileURL) as URL?, url.isFileURL {
                return shellEscape(url.path)
            }
            return item.string(forType: .string)
        }
        return strings.isEmpty ? nil : strings.joined(separator: " ")
    }

    static func shellEscape(_ s: String) -> String {
        var out = ""
        for ch in s {
            if "\\ ()[]{}<>\"'`!#$&;|*?\t".contains(ch) { out.append("\\") }
            out.append(ch)
        }
        return out
    }

    static func data(_ pb: NSPasteboard, mime: String) -> Data? {
        switch mime {
        case "text/plain":
            return string(pb).map { Data($0.utf8) }
        case "text/uri-list":
            let urls = fileURLs(pb)
            return urls.isEmpty ? nil : Data(urls.map { $0.absoluteString + "\r\n" }.joined().utf8)
        default:
            return type(forMime: mime).flatMap { pb.data(forType: $0) }
        }
    }

    static func availableMimes(_ pb: NSPasteboard) -> [String] {
        var result: [String] = []
        var seen = Set<String>()
        let types = pb.types ?? []
        func mime(_ t: NSPasteboard.PasteboardType) -> String? {
            guard let m = UTType(t.rawValue)?.preferredMIMEType else { return nil }
            return m == "text/plain;charset=utf-8" ? "text/plain" : m
        }
        let hasFile = types.contains(.fileURL)
        if hasFile || types.contains(where: { mime($0) == "text/plain" }) {
            result.append("text/plain"); seen.insert("text/plain")
        }
        if hasFile { result.append("text/uri-list"); seen.insert("text/uri-list") }
        for t in types {
            guard let m = mime(t), seen.insert(m).inserted else { continue }
            result.append(m)
        }
        return result
    }

    static func write(_ pb: NSPasteboard, _ items: [Content]) {
        pb.declareTypes(items.compactMap { type(forMime: $0.mime) }, owner: nil)
        for item in items {
            guard let t = type(forMime: item.mime) else { continue }
            pb.setData(item.data, forType: t)
        }
    }

    static func complete(
        _ surface: ghostty_surface_t,
        contents: [Content],
        available: [String],
        state: UnsafeMutableRawPointer?,
        confirmed: Bool = false,
        remember: Bool = false
    ) {
        var strings: [UnsafeMutablePointer<CChar>] = []
        var buffers: [UnsafeMutableRawPointer] = []
        defer {
            strings.forEach { free($0) }
            buffers.forEach { $0.deallocate() }
        }
        var cContents: [ghostty_clipboard_content_s] = []
        for c in contents {
            guard let mime = strdup(c.mime) else { continue }
            strings.append(mime)
            let buf = UnsafeMutableRawPointer.allocate(byteCount: max(c.data.count, 1), alignment: 1)
            buffers.append(buf)
            c.data.withUnsafeBytes { src in
                if let base = src.baseAddress { buf.copyMemory(from: base, byteCount: src.count) }
            }
            cContents.append(.init(mime: mime, data: buf.assumingMemoryBound(to: CChar.self), len: c.data.count))
        }
        var cAvail: [UnsafePointer<CChar>?] = []
        for m in available {
            guard let p = strdup(m) else { continue }
            strings.append(p)
            cAvail.append(UnsafePointer(p))
        }
        cContents.withUnsafeBufferPointer { cb in
            cAvail.withUnsafeBufferPointer { ab in
                var complete = ghostty_clipboard_complete_s(
                    contents: cb.baseAddress, contents_len: cb.count,
                    available: ab.baseAddress, available_len: ab.count,
                    confirmed: confirmed, remember: remember)
                ghostty_surface_complete_clipboard_request(surface, &complete, state)
            }
        }
    }
}

// MARK: - URLs

enum URLOpener {
    static let trustedSchemes: Set<String> = ["http", "https", "mailto", "ftp", "sftp", "ssh", "vnc", "x-man-page"]

    static func open(_ s: String, kind: ghostty_action_open_url_kind_e, in window: NSWindow? = nil) {
        let url: URL
        if let u = URL(string: s), u.scheme != nil {
            url = u
        } else {
            url = URL(fileURLWithPath: (s as NSString).standardizingPath)
        }

        // Config files etc. requested by Ghostty itself.
        if kind == GHOSTTY_ACTION_OPEN_URL_KIND_TEXT {
            let app = NSWorkspace.shared.urlForApplication(toOpen: url)
                ?? NSWorkspace.shared.urlForApplication(withBundleIdentifier: "com.apple.TextEdit")
            if let app {
                NSWorkspace.shared.open([url], withApplicationAt: app, configuration: NSWorkspace.OpenConfiguration())
                return
            }
        }

        let scheme = url.scheme?.lowercased() ?? ""
        if kind == GHOSTTY_ACTION_OPEN_URL_KIND_HTML || trustedSchemes.contains(scheme) {
            NSWorkspace.shared.open(url)
            return
        }

        // Untrusted (OSC 8 / unknown scheme / local file): confirm first.
        Alerts.confirm(
            in: window ?? NSApp.keyWindow, title: "Open this link?",
            info: "A program in the terminal wants to open:", preview: url.absoluteString,
            ok: "Open", remember: false
        ) { ok, _ in
            if ok { NSWorkspace.shared.open(url) }
        }
    }
}

// MARK: - Notifications

enum Notifier {
    private static var authorized: Bool?

    static func post(title: String, body: String) {
        // UNUserNotificationCenter requires a real app bundle.
        guard Bundle.main.bundleIdentifier != nil else { return }
        let center = UNUserNotificationCenter.current()
        let send = {
            let content = UNMutableNotificationContent()
            content.title = title.isEmpty ? "Ghostdrop" : title
            content.body = body
            center.add(UNNotificationRequest(identifier: UUID().uuidString, content: content, trigger: nil))
        }
        if authorized == true { send(); return }
        center.requestAuthorization(options: [.alert, .sound]) { ok, _ in
            DispatchQueue.main.async {
                authorized = ok
                if ok { send() }
            }
        }
    }
}

// MARK: - Secure input

/// Secure keyboard entry (EnableSecureEventInput) is global system state: while
/// on, other apps can't see keystrokes, and it stays on while we're in the
/// background. So it's only on while Ghostdrop is active AND it's wanted
/// globally (app-level toggle) or by a focused surface at a password prompt.
/// Surfaces leave the set when they go away, so a closed tab can't leave it on.
final class SecureInput {
    static let shared = SecureInput()

    private var global = false
    /// Surfaces at a password prompt, and whether each is focused.
    private var scoped: [ObjectIdentifier: Bool] = [:]
    private var appActive = NSApp.isActive
    /// EnableSecureEventInput is in effect (balanced with Disable).
    private(set) var enabled = false

    private init() {
        let nc = NotificationCenter.default
        nc.addObserver(forName: NSApplication.didBecomeActiveNotification, object: nil, queue: .main) { [weak self] _ in
            self?.appActive = true
            self?.apply()
        }
        nc.addObserver(forName: NSApplication.didResignActiveNotification, object: nil, queue: .main) { [weak self] _ in
            self?.appActive = false
            self?.apply()
        }
    }

    func setGlobal(_ mode: ghostty_action_secure_input_e) {
        switch mode {
        case GHOSTTY_SECURE_INPUT_ON: global = true
        case GHOSTTY_SECURE_INPUT_OFF: global = false
        default: global.toggle()
        }
        apply()
    }

    func setScoped(_ id: ObjectIdentifier, focused: Bool) {
        scoped[id] = focused
        apply()
    }

    func removeScoped(_ id: ObjectIdentifier) {
        guard scoped.removeValue(forKey: id) != nil else { return }
        apply()
    }

    private func apply() {
        let want = appActive && (global || scoped.values.contains(true))
        guard want != enabled else { return }
        let err = want ? EnableSecureEventInput() : DisableSecureEventInput()
        if err == noErr {
            enabled = want
        } else {
            NSLog("ghostdrop: secure input %@ failed (%d)", want ? "enable" : "disable", err)
        }
    }
}

// MARK: - Alerts

enum Alerts {
    /// Shows a confirmation as a sheet when a window is available (so the
    /// drop-down doesn't auto-hide), otherwise as a modal alert.
    static func confirm(
        in window: NSWindow?,
        title: String,
        info: String,
        preview: String? = nil,
        ok: String,
        remember: Bool,
        completion: @escaping (_ ok: Bool, _ remember: Bool) -> Void
    ) {
        let alert = NSAlert()
        alert.messageText = title
        alert.informativeText = info
        alert.alertStyle = .warning
        alert.addButton(withTitle: ok)
        alert.addButton(withTitle: "Cancel")
        if let preview, !preview.isEmpty {
            let shown = preview.count > 2000 ? String(preview.prefix(2000)) + "…" : preview
            let scroll = NSScrollView(frame: NSRect(x: 0, y: 0, width: 420, height: 140))
            scroll.hasVerticalScroller = true
            scroll.borderType = .bezelBorder
            let tv = NSTextView(frame: scroll.bounds)
            tv.isEditable = false
            tv.font = .monospacedSystemFont(ofSize: 11, weight: .regular)
            tv.string = shown
            tv.autoresizingMask = [.width]
            scroll.documentView = tv
            alert.accessoryView = scroll
        }
        if remember {
            alert.showsSuppressionButton = true
            alert.suppressionButton?.title = "Remember my choice for this session"
        }
        let handler: (NSApplication.ModalResponse) -> Void = { resp in
            completion(resp == .alertFirstButtonReturn, alert.suppressionButton?.state == .on)
        }
        if let window, window.isVisible {
            alert.beginSheetModal(for: window, completionHandler: handler)
        } else {
            NSApp.activate(ignoringOtherApps: true)
            handler(alert.runModal())
        }
    }
}
