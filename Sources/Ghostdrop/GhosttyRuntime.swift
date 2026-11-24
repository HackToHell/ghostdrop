import AppKit
import Carbon.HIToolbox
import UniformTypeIdentifiers
import UserNotifications

/// Receives libghostty actions and confirmation requests that need app/window
/// level handling. Always invoked on the main thread outside any FFI call.
protocol GhosttyRuntimeDelegate: AnyObject {
    func runtime(_ rt: GhosttyRuntime, perform action: RuntimeAction, from surface: SurfaceView?) -> Bool
    func runtimeConfigDidChange(_ rt: GhosttyRuntime)
    func runtime(
        _ rt: GhosttyRuntime,
        confirm title: String,
        info: String,
        preview: String?,
        ok: String,
        remember: Bool,
        from surface: SurfaceView?,
        completion: @escaping (_ ok: Bool, _ remember: Bool) -> Void
    )
}

/// App/window level actions forwarded from libghostty.
enum RuntimeAction {
    case newTab
    case closeTab(CloseTabMode)
    case closeWindow
    case gotoTab(TabTarget)
    case moveTab(Int)
    case toggle
    case toggleFullscreen
    case resetWindowSize
    case quit
    case reloadConfig(soft: Bool)
    case openConfig
    case promptTitle
    case floatWindow(FloatWindowMode)
    case startSearch(String)
    case endSearch
    case searchTotal(Int)
    case searchSelected(Int)
    case copyTitle
}

final class GhosttyRuntime {
    private final class WeakSurface {
        weak var view: SurfaceView?
        init(_ view: SurfaceView) { self.view = view }
    }

    private let app: GhosttyApp
    private(set) var config: GhosttyConfig
    weak var delegate: GhosttyRuntimeDelegate?
    private var surfaces: [SurfaceID: WeakSurface] = [:]
    private var observers: [NSObjectProtocol] = []

    init?(settings: Settings) {
        guard let cfg = Self.makeConfig(settings) else { return nil }
        guard let app = GhosttyApp(config: cfg) else { return nil }
        self.config = cfg
        self.app = app

        app.onEvent = { [weak self] ev in
            self?.handle(ev)
        }
        app.setFocus(NSApp.isActive)

        let nc = NotificationCenter.default
        observers.append(nc.addObserver(forName: NSApplication.didBecomeActiveNotification, object: nil, queue: .main) { [weak self] _ in
            self?.app.setFocus(true)
        })
        observers.append(nc.addObserver(forName: NSApplication.didResignActiveNotification, object: nil, queue: .main) { [weak self] _ in
            self?.app.setFocus(false)
        })
        observers.append(nc.addObserver(forName: NSTextInputContext.keyboardSelectionDidChangeNotification, object: nil, queue: .main) { [weak self] _ in
            self?.app.keyboardChanged()
        })
    }

    deinit {
        let nc = NotificationCenter.default
        observers.forEach { nc.removeObserver($0) }
    }

    // MARK: Surfaces

    func createSurface(
        for view: SurfaceView,
        profile: Profile,
        workingDirectory: String?,
        fontSize: Float,
        scaleFactor: Double
    ) -> GhosttySurface? {
        let wd = profile.workingDirectory ?? workingDirectory
        let env = profile.env.map { ($0.key, $0.value) }
        guard let surface = app.createSurface(
            nsView: Unmanaged.passUnretained(view).toOpaque(),
            scaleFactor: scaleFactor,
            fontSize: fontSize,
            workingDirectory: wd,
            command: profile.effectiveCommand,
            env: env,
            waitAfterCommand: profile.waitAfterCommand
        ) else { return nil }
        surfaces[surface.id] = WeakSurface(view)
        return surface
    }

    func unregisterSurface(_ id: SurfaceID) {
        surfaces.removeValue(forKey: id)
    }

    private func surfaceView(for id: SurfaceID?) -> SurfaceView? {
        guard let id, let v = surfaces[id]?.view, v.surface?.isAlive == true else { return nil }
        return v
    }

    // MARK: App / Config

    /// Layering: Ghostdrop defaults < your Ghostty config < Ghostdrop config.
    static func makeConfig(_ settings: Settings) -> GhosttyConfig? {
        guard let loaded = GhosttyConfig.load(
            defaultConfig: defaultGhosttyConfig,
            inheritGhosttyConfig: settings.inheritGhosttyConfig,
            hasGhosttyConfig: hasGhosttyConfig,
            overrideLines: settings.ghosttyLines
        ) else { return nil }
        settings.errors.append(contentsOf: loaded.diagnostics)
        return loaded.config
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
        guard let cfg = Self.makeConfig(settings) else { return }
        app.updateConfig(cfg)
        config = cfg
        delegate?.runtimeConfigDidChange(self)
    }

    func softReload() {
        app.updateConfig(config)
    }

    var needsConfirmQuit: Bool { app.needsConfirmQuit }

    func setColorScheme(dark: Bool) {
        app.setColorScheme(dark: dark)
    }

    func setWindowBackgroundBlur(_ window: NSWindow) {
        app.setWindowBackgroundBlur(nsWindow: Unmanaged.passUnretained(window).toOpaque())
    }

    func string(_ key: String) -> String? { config.string(key) }
    func double(_ key: String, _ def: Double) -> Double { config.double(key, default: def) }
    func bool(_ key: String, _ def: Bool) -> Bool { config.bool(key, default: def) }
    func uint(_ key: String) -> UInt32 { config.uint(key) }

    func color(_ key: String) -> NSColor? {
        config.color(key).map { NSColor(srgbRed: CGFloat($0.r) / 255, green: CGFloat($0.g) / 255, blue: CGFloat($0.b) / 255, alpha: 1) }
    }

    var quickTerminalSize: QuickTerminalSize { config.quickTerminalSize }

    // MARK: Event dispatch (always on main thread, outside libghostty)

    private func handle(_ event: GhosttyEvent) {
        GhosttyFFI.assertNotInCall()

        func forward(_ action: RuntimeAction, from sid: SurfaceID? = nil) {
            _ = delegate?.runtime(self, perform: action, from: surfaceView(for: sid))
        }

        func requestConfirm(
            title: String,
            info: String,
            preview: String?,
            ok: String,
            remember: Bool,
            from view: SurfaceView?,
            completion: @escaping (_ ok: Bool, _ remember: Bool) -> Void
        ) {
            if let delegate {
                delegate.runtime(self, confirm: title, info: info, preview: preview, ok: ok, remember: remember, from: view, completion: completion)
            } else {
                Alerts.presentConfirmation(title: title, info: info, preview: preview, ok: ok, remember: remember, completion: completion)
            }
        }

        switch event {
        case .newTab(let sid): forward(.newTab, from: sid)
        case .closeTab(let sid, let mode): forward(.closeTab(mode), from: sid)
        case .closeWindow: forward(.closeWindow)
        case .gotoTab(let target): forward(.gotoTab(target))
        case .moveTab(let n): forward(.moveTab(n))
        case .toggle: forward(.toggle)
        case .toggleFullscreen: forward(.toggleFullscreen)
        case .resetWindowSize: forward(.resetWindowSize)
        case .quit: forward(.quit)
        case .reloadConfig(let soft): forward(.reloadConfig(soft: soft))
        case .openConfig: forward(.openConfig)
        case .promptTitle(let sid): forward(.promptTitle, from: sid)
        case .floatWindow(let mode): forward(.floatWindow(mode))
        case .copyTitle(let sid): forward(.copyTitle, from: sid)
        case .startSearch(let sid, let needle): forward(.startSearch(needle), from: sid)
        case .endSearch(let sid): forward(.endSearch, from: sid)
        case .searchTotal(let sid, let n): forward(.searchTotal(n), from: sid)
        case .searchSelected(let sid, let n): forward(.searchSelected(n), from: sid)

        case .setTitle(let sid, let title):
            surfaceView(for: sid)?.setTitleFromTerminal(title)

        case .pwd(let sid, let pwd):
            surfaceView(for: sid)?.pwd = pwd

        case .mouseShape(let sid, let shape):
            surfaceView(for: sid)?.setCursorShape(shape)

        case .mouseVisibility(let visible):
            NSCursor.setHiddenUntilMouseMoves(!visible)

        case .mouseOverLink(let sid, let url):
            surfaceView(for: sid)?.hoverURL = url

        case .cellSize(let sid, let w, let h):
            surfaceView(for: sid)?.cellSize = NSSize(width: CGFloat(w), height: CGFloat(h))

        case .ringBell(let sid):
            guard sid == nil || surfaceView(for: sid) != nil else { return }
            if uint("bell-features") & 0b1 != 0 { NSSound.beep() }
            surfaceView(for: sid)?.ringBell()

        case .openURL(let sid, let urlString, let kind):
            let view = surfaceView(for: sid)
            guard sid == nil || view != nil else { return }
            URLOpener.open(urlString, kind: kind) { url in
                requestConfirm(
                    title: "Open this link?",
                    info: "A program in the terminal wants to open:",
                    preview: url.absoluteString,
                    ok: "Open",
                    remember: false,
                    from: view
                ) { ok, _ in
                    if ok { NSWorkspace.shared.open(url) }
                }
            }

        case .desktopNotification(let title, let body):
            Notifier.post(title: title, body: body)

        case .backgroundColor(let sid, let c):
            let color = NSColor(srgbRed: CGFloat(c.r) / 255, green: CGFloat(c.g) / 255, blue: CGFloat(c.b) / 255, alpha: 1)
            surfaceView(for: sid)?.backgroundColorOverride = color

        case .secureInput(let sid, let mode):
            if let sid {
                guard let view = surfaceView(for: sid) else { return }
                guard mode == .toggle || bool("macos-auto-secure-input", true) else { return }
                view.setPasswordInput(mode)
            } else {
                SecureInput.shared.setGlobal(mode)
            }

        case .rendererUnhealthy:
            NSLog("ghostdrop: renderer reported unhealthy")

        case .closeSurface(let sid, let processAlive):
            guard let view = surfaceView(for: sid) else { return }
            view.owner?.surfaceRequestedClose(view, needsConfirm: processAlive)

        case .clipboardRead(let sid, let loc, let token, let mimes, let list):
            guard let view = surfaceView(for: sid), let surface = view.surface else { return }
            guard let pb = Clipboard.pasteboard(loc) else {
                surface.denyClipboard(token: token)
                return
            }
            var contents: [ClipboardContent] = []
            var seen = Set<String>()
            for mime in mimes where seen.insert(mime).inserted {
                if let data = Clipboard.data(pb, mime: mime) {
                    contents.append(ClipboardContent(mime: mime, data: data))
                }
            }
            let available = list ? Clipboard.availableMimes(pb) : []
            if contents.isEmpty && !list {
                // Nothing to disclose: complete empty without triggering an ask prompt.
                surface.completeClipboard(contents: [], available: [], token: token, confirmed: true, remember: false)
            } else {
                surface.completeClipboard(contents: contents, available: available, token: token, confirmed: false, remember: false)
            }

        case .clipboardConfirmNil(let sid, let token):
            surfaceView(for: sid)?.surface?.denyClipboard(token: token)

        case .clipboardConfirm(let sid, let contents, let available, let canRemember, let token, let kind):
            guard let view = surfaceView(for: sid), view.surface?.isAlive == true else { return }
            let text = contents.first(where: { $0.mime == "text/plain" }).flatMap { String(data: $0.data, encoding: .utf8) }
                ?? contents.map { "\($0.mime) (\($0.data.count) bytes)" }.joined(separator: "\n")
            let (title, info): (String, String) = {
                switch kind {
                case .paste:
                    return ("Potentially unsafe paste", "The text you're pasting contains newlines or control sequences that could run commands.")
                case .osc52Read, .kittyRead, .list:
                    return ("Allow clipboard read?", "A program in this terminal wants to read your clipboard.")
                default:
                    return ("Allow clipboard access?", "A program in this terminal is requesting clipboard access.")
                }
            }()
            requestConfirm(
                title: title,
                info: info,
                preview: text,
                ok: kind == .paste ? "Paste" : "Allow",
                remember: canRemember,
                from: view
            ) { allowed, remember in
                guard let surface = view.surface else { return }
                if allowed {
                    surface.completeClipboard(contents: contents, available: available, token: token, confirmed: true, remember: remember)
                } else {
                    surface.denyClipboard(token: token)
                }
            }

        case .clipboardWrite(let sid, let loc, let contents, let confirm):
            guard let pb = Clipboard.pasteboard(loc) else { return }
            if !confirm {
                Clipboard.write(pb, contents)
                return
            }
            let view = surfaceView(for: sid)
            guard sid == nil || view != nil else { return }
            guard let text = contents.first(where: { $0.mime == "text/plain" }).flatMap({ String(data: $0.data, encoding: .utf8) }) else { return }
            requestConfirm(
                title: "Allow clipboard write?",
                info: "A program in this terminal wants to set your clipboard to:",
                preview: text,
                ok: "Allow",
                remember: false,
                from: view
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
    static let selection = NSPasteboard(name: .init("com.ghostdrop.selection"))

    static func pasteboard(_ loc: ClipboardLocation) -> NSPasteboard? {
        switch loc {
        case .standard: return .general
        case .selection: return selection
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

    static func write(_ pb: NSPasteboard, _ items: [ClipboardContent]) {
        pb.declareTypes(items.compactMap { type(forMime: $0.mime) }, owner: nil)
        for item in items {
            guard let t = type(forMime: item.mime) else { continue }
            pb.setData(item.data, forType: t)
        }
    }
}

// MARK: - URLs

enum URLOpener {
    static let trustedSchemes: Set<String> = ["http", "https", "mailto", "ftp", "sftp", "ssh", "vnc", "x-man-page"]

    static func open(_ s: String, kind: OpenURLKind, confirmUntrusted: (URL) -> Void) {
        let url: URL
        if let u = URL(string: s), u.scheme != nil {
            url = u
        } else {
            url = URL(fileURLWithPath: (s as NSString).standardizingPath)
        }

        // Config files etc. requested by Ghostty itself.
        if kind == .text {
            let app = NSWorkspace.shared.urlForApplication(toOpen: url)
                ?? NSWorkspace.shared.urlForApplication(withBundleIdentifier: "com.apple.TextEdit")
            if let app {
                NSWorkspace.shared.open([url], withApplicationAt: app, configuration: NSWorkspace.OpenConfiguration())
                return
            }
        }

        let scheme = url.scheme?.lowercased() ?? ""
        if kind == .html || trustedSchemes.contains(scheme) {
            NSWorkspace.shared.open(url)
            return
        }

        // Untrusted (OSC 8 / unknown scheme / local file): confirm first.
        confirmUntrusted(url)
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

    func setGlobal(_ mode: SecureInputMode) {
        switch mode {
        case .on: global = true
        case .off: global = false
        case .toggle: global.toggle()
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

/// Builds confirmation alerts and presents standalone alerts without entering a
/// nested modal runloop (`NSAlert.runModal()`).
enum Alerts {
    private final class WindowHost: NSObject, NSWindowDelegate {
        static var active: [ObjectIdentifier: WindowHost] = [:]

        private let alert: NSAlert
        private let completion: ((NSApplication.ModalResponse) -> Void)?
        private var finished = false

        init(alert: NSAlert, completion: ((NSApplication.ModalResponse) -> Void)?) {
            self.alert = alert
            self.completion = completion
            super.init()
        }

        @objc func buttonPressed(_ sender: NSButton) {
            finish(NSApplication.ModalResponse(rawValue: sender.tag))
        }

        func windowWillClose(_ notification: Notification) {
            finish(.abort)
        }

        func finish(_ response: NSApplication.ModalResponse) {
            guard !finished else { return }
            finished = true
            alert.window.delegate = nil
            alert.window.orderOut(nil)
            Self.active.removeValue(forKey: ObjectIdentifier(self))
            completion?(response)
        }
    }

    static var activeAlertCount: Int { WindowHost.active.count }

    static func makeConfirmation(
        title: String,
        info: String,
        preview: String? = nil,
        ok: String,
        remember: Bool
    ) -> NSAlert {
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
        return alert
    }

    /// Presents `alert` as a normal non-modal window (no `runModal()` nested runloop).
    static func present(
        _ alert: NSAlert,
        activate: Bool = true,
        completion: ((NSApplication.ModalResponse) -> Void)? = nil
    ) {
        GhosttyFFI.assertNotInCall()
        let host = WindowHost(alert: alert, completion: completion)
        WindowHost.active[ObjectIdentifier(host)] = host
        alert.layout()
        for (i, button) in alert.buttons.enumerated() {
            button.tag = NSApplication.ModalResponse.alertFirstButtonReturn.rawValue + i
            button.target = host
            button.action = #selector(WindowHost.buttonPressed(_:))
        }
        let w = alert.window
        w.delegate = host
        w.center()
        if activate { NSApp.activate(ignoringOtherApps: true) }
        w.makeKeyAndOrderFront(nil)
    }

    static func presentConfirmation(
        title: String,
        info: String,
        preview: String? = nil,
        ok: String,
        remember: Bool,
        completion: @escaping (_ ok: Bool, _ remember: Bool) -> Void
    ) {
        let alert = makeConfirmation(title: title, info: info, preview: preview, ok: ok, remember: remember)
        present(alert) { resp in
            completion(resp == .alertFirstButtonReturn, alert.suppressionButton?.state == .on)
        }
    }
}
