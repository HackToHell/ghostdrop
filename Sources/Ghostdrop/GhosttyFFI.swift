import CoreText
import Foundation
import GhosttyKit

/// Unique identifier for a libghostty surface. Passed as opaque userdata so C
/// callbacks never hold or dereference AppKit objects.
typealias SurfaceID = UInt64

// MARK: - Pure Swift FFI value types

struct GhosttyColor: Equatable {
    let r: UInt8
    let g: UInt8
    let b: UInt8
}

enum QuickTerminalDimension: Equatable {
    case percentage(Double)
    case pixels(Double)
}

struct QuickTerminalSize: Equatable {
    let primary: QuickTerminalDimension?
    let secondary: QuickTerminalDimension?
}

enum CloseTabMode: Equatable {
    case this
    case other
    case right
}

enum TabTarget: Equatable {
    case previous
    case next
    case last
    case index(Int)

    init?(raw: Int32) {
        switch raw {
        case GHOSTTY_GOTO_TAB_PREVIOUS.rawValue: self = .previous
        case GHOSTTY_GOTO_TAB_NEXT.rawValue: self = .next
        case GHOSTTY_GOTO_TAB_LAST.rawValue: self = .last
        default:
            guard raw >= 1 else { return nil }
            self = .index(Int(raw) - 1)
        }
    }
}

enum FloatWindowMode: Equatable {
    case on
    case off
    case toggle
}

enum OpenURLKind: Equatable {
    case unknown
    case text
    case html
    case osc8
}

enum SecureInputMode: Equatable {
    case on
    case off
    case toggle
}

enum MouseShape: Equatable {
    case defaultArrow
    case text
    case pointer
    case grab
    case grabbing
    case crosshair
    case verticalText
    case contextMenu
    case notAllowed
    case resizeEW
    case resizeNS
    case resizeW
    case resizeE
    case resizeN
    case resizeS
}

enum ClipboardLocation: Equatable {
    case standard
    case selection
}

struct ClipboardContent: Equatable {
    let mime: String
    let data: Data
}

enum ClipboardRequestKind: Equatable {
    case paste
    case osc52Read
    case osc52Write
    case kittyRead
    case kittyWrite
    case list
    case other
}

/// Opaque handle for an in-flight libghostty clipboard request.
struct ClipboardRequestToken: Hashable {
    fileprivate let raw: UInt

    fileprivate init(_ ptr: UnsafeMutableRawPointer?) {
        raw = UInt(bitPattern: ptr)
    }

    fileprivate var pointer: UnsafeMutableRawPointer? {
        UnsafeMutableRawPointer(bitPattern: raw)
    }
}

enum KeyAction: Equatable {
    case release
    case press
    case repeatKey
}

struct GhosttyMods: OptionSet {
    let rawValue: UInt32

    static let shift      = GhosttyMods(rawValue: 1 << 0)
    static let ctrl       = GhosttyMods(rawValue: 1 << 1)
    static let alt        = GhosttyMods(rawValue: 1 << 2)
    static let superMod   = GhosttyMods(rawValue: 1 << 3)
    static let caps       = GhosttyMods(rawValue: 1 << 4)
    static let num        = GhosttyMods(rawValue: 1 << 5)
    static let shiftRight = GhosttyMods(rawValue: 1 << 6)
    static let ctrlRight  = GhosttyMods(rawValue: 1 << 7)
    static let altRight   = GhosttyMods(rawValue: 1 << 8)
    static let superRight = GhosttyMods(rawValue: 1 << 9)
}

struct GhosttyKeyEvent {
    var action: KeyAction
    var mods: GhosttyMods
    var consumedMods: GhosttyMods
    var keycode: UInt32
    var text: String?
    var unshiftedCodepoint: UInt32
    var composing: Bool
}

enum MouseButtonState {
    case release
    case press
}

enum MouseButton {
    case unknown
    case left
    case right
    case middle
    case four
    case five
    case six
    case seven
    case eight
    case nine
    case ten
    case eleven
}

enum ScrollMomentum: UInt8 {
    case none = 0
    case began = 1
    case stationary = 2
    case changed = 3
    case ended = 4
    case cancelled = 5
    case mayBegin = 6
}

struct SurfaceGridSize {
    let cols: Int
    let rows: Int
}

struct InheritedSurfaceConfig {
    let workingDirectory: String?
    let fontSize: Float
}

struct SurfaceTextResult {
    let text: String
    let tlPxX: Double
    let tlPxY: Double
    let offsetStart: Int
    let offsetLen: Int
}

struct SurfaceIMEPoint {
    let x: Double
    let y: Double
    let width: Double
    let height: Double
}

/// Pure-data events produced by libghostty C callbacks and drained on the main
/// runloop after libghostty has unwound and released its internal locks.
enum GhosttyEvent {
    case newTab(SurfaceID?)
    case closeTab(SurfaceID?, CloseTabMode)
    case closeWindow
    case gotoTab(TabTarget)
    case moveTab(Int)
    case toggle
    case toggleFullscreen
    case resetWindowSize
    case quit
    case reloadConfig(soft: Bool)
    case openConfig
    case promptTitle(SurfaceID?)
    case floatWindow(FloatWindowMode)
    case copyTitle(SurfaceID?)
    case startSearch(SurfaceID?, String)
    case endSearch(SurfaceID?)
    case searchTotal(SurfaceID?, Int)
    case searchSelected(SurfaceID?, Int)
    case setTitle(SurfaceID?, String)
    case pwd(SurfaceID?, String?)
    case mouseShape(SurfaceID?, MouseShape)
    case mouseVisibility(Bool)
    case mouseOverLink(SurfaceID?, String?)
    case cellSize(SurfaceID?, width: Double, height: Double)
    case ringBell(SurfaceID?)
    case openURL(SurfaceID?, String, OpenURLKind)
    case desktopNotification(title: String, body: String)
    case backgroundColor(SurfaceID?, GhosttyColor)
    case secureInput(SurfaceID?, SecureInputMode)
    case rendererUnhealthy
    case closeSurface(SurfaceID, processAlive: Bool)
    case clipboardRead(SurfaceID, ClipboardLocation, ClipboardRequestToken, mimes: [String], list: Bool)
    case clipboardConfirm(SurfaceID, contents: [ClipboardContent], available: [String], canRemember: Bool, token: ClipboardRequestToken, kind: ClipboardRequestKind)
    case clipboardConfirmNil(SurfaceID, ClipboardRequestToken)
    case clipboardWrite(SurfaceID?, ClipboardLocation, contents: [ClipboardContent], confirm: Bool)
}

// MARK: - Re-entrancy guard & global init

/// Single FFI boundary to `GhosttyKit`. No other file imports `GhosttyKit`, and
/// this file does not import `AppKit`, making AppKit/sheet/focus re-entrancy
/// from inside libghostty callbacks structurally impossible.
enum GhosttyFFI {
    private static let callbackKey: pthread_key_t = {
        var key: pthread_key_t = 0
        pthread_key_create(&key, nil)
        return key
    }()

    /// Depth of active main-thread calls into libghostty.
    private static var ffiDepth: Int = 0

    static var isInCallbackOnCurrentThread: Bool {
        pthread_getspecific(callbackKey) != nil
    }

    static var isInCall: Bool {
        isInCallbackOnCurrentThread || (Thread.isMainThread && ffiDepth > 0)
    }

    /// Traps immediately if called from inside a libghostty callback or while a
    /// libghostty FFI call is still active on the main thread.
    static func assertNotInCall(_ context: StaticString = #function) {
        precondition(!isInCallbackOnCurrentThread, "Re-entrant call in \(context) from inside a libghostty callback")
        precondition(!(Thread.isMainThread && ffiDepth > 0), "Re-entrant call in \(context) while a libghostty FFI call is active")
    }

    @inline(__always)
    fileprivate static func withFFI<R>(_ context: StaticString = #function, _ body: () -> R) -> R {
        precondition(Thread.isMainThread, "libghostty FFI (\(context)) must run on the main thread")
        assertNotInCall(context)
        ffiDepth += 1
        defer { ffiDepth -= 1 }
        return body()
    }

    @inline(__always)
    fileprivate static func withCallback<R>(_ body: () -> R) -> R {
        let prev = Int(bitPattern: pthread_getspecific(callbackKey))
        pthread_setspecific(callbackKey, UnsafeMutableRawPointer(bitPattern: prev + 1))
        defer { pthread_setspecific(callbackKey, UnsafeMutableRawPointer(bitPattern: prev)) }
        return body()
    }

    /// Retained for process lifetime: `ghostty_init` stores `argv` inside global state.
    private static var globalArgv: UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>?

    static func initGlobal(args: [String]) -> Bool {
        withFFI {
            let buf = UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>.allocate(capacity: args.count + 1)
            for (i, arg) in args.enumerated() { buf[i] = strdup(arg) }
            buf[args.count] = nil
            globalArgv = buf
            return ghostty_init(UInt(args.count), buf) == GHOSTTY_SUCCESS
        }
    }

    static func runCLIAction() {
        withFFI {
            ghostty_cli_try_action()
        }
    }
}

// MARK: - Config wrapper

final class GhosttyConfig {
    fileprivate let raw: ghostty_config_t

    private init(raw: ghostty_config_t) {
        self.raw = raw
    }

    deinit {
        let raw = self.raw
        if Thread.isMainThread && !GhosttyFFI.isInCall {
            GhosttyFFI.withFFI { ghostty_config_free(raw) }
        } else {
            DispatchQueue.main.async {
                GhosttyFFI.withFFI { ghostty_config_free(raw) }
            }
        }
    }

    static func load(
        defaultConfig: String,
        inheritGhosttyConfig: Bool,
        hasGhosttyConfig: Bool,
        overrideLines: [String]
    ) -> (config: GhosttyConfig, diagnostics: [String])? {
        GhosttyFFI.withFFI {
            guard let cfg = ghostty_config_new() else { return nil }
            let dir = (NSTemporaryDirectory() as NSString).appendingPathComponent("ghostdrop-\(getuid())")
            try? FileManager.default.createDirectory(atPath: dir, withIntermediateDirectories: true)

            let defaults = (dir as NSString).appendingPathComponent("defaults.conf")
            try? defaultConfig.write(toFile: defaults, atomically: true, encoding: .utf8)
            ghostty_config_load_file(cfg, defaults)

            // Only when the user actually has a Ghostty config: otherwise libghostty
            // would create a Ghostty template file on Macs that don't use Ghostty.
            if inheritGhosttyConfig && hasGhosttyConfig {
                ghostty_config_load_default_files(cfg)
            }

            if !overrideLines.isEmpty {
                let overrides = (dir as NSString).appendingPathComponent("overrides.conf")
                let body = overrideLines.joined(separator: "\n") + "\n"
                try? body.write(toFile: overrides, atomically: true, encoding: .utf8)
                ghostty_config_load_file(cfg, overrides)
            }
            ghostty_config_load_recursive_files(cfg)
            ghostty_config_finalize(cfg)

            var diagnostics: [String] = []
            let n = ghostty_config_diagnostics_count(cfg)
            for i in 0..<n {
                let d = ghostty_config_get_diagnostic(cfg, i)
                if let m = d.message { diagnostics.append("ghostty: " + String(cString: m)) }
            }
            return (GhosttyConfig(raw: cfg), diagnostics)
        }
    }

    func string(_ key: String) -> String? {
        GhosttyFFI.withFFI {
            var v: UnsafePointer<Int8>?
            guard ghostty_config_get(raw, &v, key, UInt(key.utf8.count)), let v else { return nil }
            return String(cString: v)
        }
    }

    func double(_ key: String, default def: Double) -> Double {
        GhosttyFFI.withFFI {
            var v = def
            _ = ghostty_config_get(raw, &v, key, UInt(key.utf8.count))
            return v
        }
    }

    func bool(_ key: String, default def: Bool) -> Bool {
        GhosttyFFI.withFFI {
            var v = def
            _ = ghostty_config_get(raw, &v, key, UInt(key.utf8.count))
            return v
        }
    }

    func uint(_ key: String) -> UInt32 {
        GhosttyFFI.withFFI {
            var v: CUnsignedInt = 0
            _ = ghostty_config_get(raw, &v, key, UInt(key.utf8.count))
            return v
        }
    }

    func color(_ key: String) -> GhosttyColor? {
        GhosttyFFI.withFFI {
            var c = ghostty_config_color_s()
            guard ghostty_config_get(raw, &c, key, UInt(key.utf8.count)) else { return nil }
            return GhosttyColor(r: c.r, g: c.g, b: c.b)
        }
    }

    var quickTerminalSize: QuickTerminalSize {
        GhosttyFFI.withFFI {
            var v = ghostty_config_quick_terminal_size_s()
            let key = "quick-terminal-size"
            _ = ghostty_config_get(raw, &v, key, UInt(key.utf8.count))
            return QuickTerminalSize(primary: Self.dim(v.primary), secondary: Self.dim(v.secondary))
        }
    }

    private static func dim(_ s: ghostty_quick_terminal_size_s) -> QuickTerminalDimension? {
        switch s.tag {
        case GHOSTTY_QUICK_TERMINAL_SIZE_PERCENTAGE: return .percentage(Double(s.value.percentage))
        case GHOSTTY_QUICK_TERMINAL_SIZE_PIXELS: return .pixels(Double(s.value.pixels))
        default: return nil
        }
    }
}

// MARK: - App wrapper & pure-data C callbacks

final class GhosttyApp {
    private var raw: ghostty_app_t?
    private let lock = NSLock()
    private var tickPending = false
    private var drainScheduled = false
    private var events: [GhosttyEvent] = []
    /// Live libghostty clipboard request `state` pointers per surface. Guarantees
    /// each token is completed/denied at most once and denied before surface free.
    private var pendingClipboardStates: [SurfaceID: Set<UInt>] = [:]
    private var nextSurfaceID: SurfaceID = 1

    /// Invoked on the main thread strictly outside libghostty (`!GhosttyFFI.isInCall`).
    var onEvent: ((GhosttyEvent) -> Void)?

    init?(config: GhosttyConfig) {
        var rt = ghostty_runtime_config_s(
            userdata: Unmanaged.passUnretained(self).toOpaque(),
            supports_selection_clipboard: true,
            wakeup_cb: { ud in GhosttyApp.cbWakeup(ud) },
            action_cb: { app, target, action in GhosttyApp.cbAction(app, target: target, action: action) },
            read_clipboard_cb: { ud, loc, state, mimes, mimesLen, list in
                GhosttyApp.cbReadClipboard(ud, location: loc, state: state, mimes: mimes, mimesLen: mimesLen, list: list)
            },
            confirm_read_clipboard_cb: { ud, confirm, state, request in
                GhosttyApp.cbConfirmReadClipboard(ud, confirm: confirm, state: state, request: request)
            },
            write_clipboard_cb: { ud, loc, content, len, confirm in
                GhosttyApp.cbWriteClipboard(ud, location: loc, content: content, len: len, confirm: confirm)
            },
            close_surface_cb: { ud, alive in GhosttyApp.cbCloseSurface(ud, processAlive: alive) }
        )
        let created: ghostty_app_t? = GhosttyFFI.withFFI { ghostty_app_new(&rt, config.raw) }
        guard let created else { return nil }
        raw = created
    }

    deinit {
        if let raw {
            self.raw = nil
            if Thread.isMainThread && !GhosttyFFI.isInCall {
                GhosttyFFI.withFFI { ghostty_app_free(raw) }
            } else {
                DispatchQueue.main.async {
                    GhosttyFFI.withFFI { ghostty_app_free(raw) }
                }
            }
        }
    }

    // MARK: App FFI methods

    func setFocus(_ focused: Bool) {
        guard let raw else { return }
        GhosttyFFI.withFFI { ghostty_app_set_focus(raw, focused) }
    }

    func keyboardChanged() {
        guard let raw else { return }
        GhosttyFFI.withFFI { ghostty_app_keyboard_changed(raw) }
    }

    func updateConfig(_ config: GhosttyConfig) {
        guard let raw else { return }
        GhosttyFFI.withFFI { ghostty_app_update_config(raw, config.raw) }
    }

    var needsConfirmQuit: Bool {
        guard let raw else { return false }
        return GhosttyFFI.withFFI { ghostty_app_needs_confirm_quit(raw) }
    }

    func setColorScheme(dark: Bool) {
        guard let raw else { return }
        let scheme = dark ? GHOSTTY_COLOR_SCHEME_DARK : GHOSTTY_COLOR_SCHEME_LIGHT
        GhosttyFFI.withFFI { ghostty_app_set_color_scheme(raw, scheme) }
    }

    func setWindowBackgroundBlur(nsWindow: UnsafeMutableRawPointer) {
        guard let raw else { return }
        GhosttyFFI.withFFI { ghostty_set_window_background_blur(raw, nsWindow) }
    }

    func createSurface(
        nsView: UnsafeMutableRawPointer,
        scaleFactor: Double,
        fontSize: Float,
        workingDirectory: String?,
        command: String?,
        env: [(String, String)],
        waitAfterCommand: Bool
    ) -> GhosttySurface? {
        guard let raw else { return nil }
        let sid = nextSurfaceID
        nextSurfaceID += 1

        // Map surface ID -> self for callbacks that only receive surface userdata.
        Self.registerSurfaceApp(sid, app: self)

        let created: ghostty_surface_t? = GhosttyFFI.withFFI {
            var cfg = ghostty_surface_config_new()
            cfg.userdata = UnsafeMutableRawPointer(bitPattern: UInt(sid))
            cfg.platform_tag = GHOSTTY_PLATFORM_MACOS
            cfg.platform = ghostty_platform_u(macos: ghostty_platform_macos_s(nsview: nsView))
            cfg.scale_factor = scaleFactor
            cfg.font_size = fontSize
            cfg.wait_after_command = waitAfterCommand
            cfg.context = GHOSTTY_SURFACE_CONTEXT_TAB

            return withOptionalCString(workingDirectory) { cwd in
                withOptionalCString(command) { cmd in
                    withCStringPairs(env) { pairs in
                        var vars = pairs.map { ghostty_env_var_s(key: $0.0, value: $0.1) }
                        return vars.withUnsafeMutableBufferPointer { buf in
                            cfg.working_directory = cwd
                            cfg.command = cmd
                            cfg.env_vars = buf.count > 0 ? buf.baseAddress : nil
                            cfg.env_var_count = buf.count
                            return ghostty_surface_new(raw, &cfg)
                        }
                    }
                }
            }
        }
        guard let created else {
            Self.unregisterSurfaceApp(sid)
            return nil
        }
        return GhosttySurface(id: sid, app: self, raw: created)
    }

    // MARK: Surface -> App lookup for surface-only callbacks

    private final class WeakApp {
        weak var app: GhosttyApp?
        init(_ app: GhosttyApp) { self.app = app }
    }

    private static let surfaceAppLock = NSLock()
    private static var surfaceApps: [SurfaceID: WeakApp] = [:]

    fileprivate static func registerSurfaceApp(_ id: SurfaceID, app: GhosttyApp) {
        surfaceAppLock.lock()
        surfaceApps[id] = WeakApp(app)
        surfaceAppLock.unlock()
    }

    fileprivate static func unregisterSurfaceApp(_ id: SurfaceID) {
        surfaceAppLock.lock()
        surfaceApps.removeValue(forKey: id)
        surfaceAppLock.unlock()
    }

    private static func app(forSurfaceUserdata ud: UnsafeMutableRawPointer?) -> (SurfaceID, GhosttyApp)? {
        let sid = SurfaceID(UInt(bitPattern: ud))
        guard sid != 0 else { return nil }
        surfaceAppLock.lock()
        let ref = surfaceApps[sid]?.app
        surfaceAppLock.unlock()
        guard let ref else { return nil }
        return (sid, ref)
    }

    // MARK: Pending clipboard state tracking

    private func trackClipboardStateLocked(_ token: ClipboardRequestToken, for sid: SurfaceID) {
        guard token.raw != 0 else { return }
        pendingClipboardStates[sid, default: []].insert(token.raw)
    }

    fileprivate func consumeClipboardState(_ token: ClipboardRequestToken, for sid: SurfaceID) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        guard token.raw != 0, var set = pendingClipboardStates[sid], set.remove(token.raw) != nil else {
            return false
        }
        if set.isEmpty { pendingClipboardStates.removeValue(forKey: sid) } else { pendingClipboardStates[sid] = set }
        return true
    }

    fileprivate func takePendingClipboardStates(for sid: SurfaceID) -> [ClipboardRequestToken] {
        lock.lock()
        let set = pendingClipboardStates.removeValue(forKey: sid) ?? []
        lock.unlock()
        return set.map { ClipboardRequestToken(UnsafeMutableRawPointer(bitPattern: $0)) }
    }

    // MARK: Async queue & drain

    private func enqueue(_ event: GhosttyEvent, clipboardState: (SurfaceID, ClipboardRequestToken)? = nil) {
        lock.lock()
        if let (sid, tok) = clipboardState {
            trackClipboardStateLocked(tok, for: sid)
        }
        events.append(event)
        let schedule = !drainScheduled
        drainScheduled = true
        lock.unlock()
        if schedule {
            DispatchQueue.main.async { [weak self] in self?.drainLoop() }
        }
    }

    private func enqueueWakeup() {
        lock.lock()
        tickPending = true
        let schedule = !drainScheduled
        drainScheduled = true
        lock.unlock()
        if schedule {
            DispatchQueue.main.async { [weak self] in self?.drainLoop() }
        }
    }

    private func drainLoop() {
        GhosttyFFI.assertNotInCall()
        // Cap passes per runloop turn so a burst of wakeups cannot starve AppKit.
        for _ in 0..<8 {
            lock.lock()
            let doTick = tickPending
            tickPending = false
            lock.unlock()

            if doTick, let raw {
                GhosttyFFI.withFFI { ghostty_app_tick(raw) }
            }

            lock.lock()
            let batch = events
            events.removeAll(keepingCapacity: true)
            if batch.isEmpty {
                if !tickPending {
                    drainScheduled = false
                    lock.unlock()
                    return
                }
                lock.unlock()
                continue
            }
            lock.unlock()

            for ev in batch {
                GhosttyFFI.assertNotInCall()
                onEvent?(ev)
            }
        }

        lock.lock()
        let more = tickPending || !events.isEmpty
        drainScheduled = more
        lock.unlock()
        if more {
            DispatchQueue.main.async { [weak self] in self?.drainLoop() }
        }
    }

    // MARK: Pure-data C callbacks

    private static func cbWakeup(_ ud: UnsafeMutableRawPointer?) {
        GhosttyFFI.withCallback {
            guard let ud else { return }
            Unmanaged<GhosttyApp>.fromOpaque(ud).takeUnretainedValue().enqueueWakeup()
        }
    }

    private static func cbAction(_ appPtr: ghostty_app_t?, target: ghostty_target_s, action: ghostty_action_s) -> Bool {
        GhosttyFFI.withCallback {
            guard let appPtr, let ud = ghostty_app_userdata(appPtr) else { return false }
            let app = Unmanaged<GhosttyApp>.fromOpaque(ud).takeUnretainedValue()
            let sid: SurfaceID? = {
                guard target.tag == GHOSTTY_TARGET_SURFACE,
                      let s = target.target.surface,
                      let sud = ghostty_surface_userdata(s) else { return nil }
                let id = SurfaceID(UInt(bitPattern: sud))
                return id != 0 ? id : nil
            }()

            let ev: GhosttyEvent
            switch action.tag {
            case GHOSTTY_ACTION_NEW_TAB, GHOSTTY_ACTION_NEW_WINDOW:
                ev = .newTab(sid)
            case GHOSTTY_ACTION_CLOSE_TAB:
                let mode: CloseTabMode
                switch action.action.close_tab_mode {
                case GHOSTTY_ACTION_CLOSE_TAB_MODE_OTHER: mode = .other
                case GHOSTTY_ACTION_CLOSE_TAB_MODE_RIGHT: mode = .right
                default: mode = .this
                }
                ev = .closeTab(sid, mode)
            case GHOSTTY_ACTION_CLOSE_WINDOW, GHOSTTY_ACTION_CLOSE_ALL_WINDOWS:
                ev = .closeWindow
            case GHOSTTY_ACTION_GOTO_TAB:
                guard let tabTarget = TabTarget(raw: action.action.goto_tab.rawValue) else { return true }
                ev = .gotoTab(tabTarget)
            case GHOSTTY_ACTION_MOVE_TAB:
                ev = .moveTab(Int(action.action.move_tab.amount))
            case GHOSTTY_ACTION_TOGGLE_QUICK_TERMINAL, GHOSTTY_ACTION_TOGGLE_VISIBILITY:
                ev = .toggle
            case GHOSTTY_ACTION_TOGGLE_FULLSCREEN, GHOSTTY_ACTION_TOGGLE_MAXIMIZE:
                ev = .toggleFullscreen
            case GHOSTTY_ACTION_RESET_WINDOW_SIZE:
                ev = .resetWindowSize
            case GHOSTTY_ACTION_QUIT:
                ev = .quit
            case GHOSTTY_ACTION_RELOAD_CONFIG:
                ev = .reloadConfig(soft: action.action.reload_config.soft)
            case GHOSTTY_ACTION_OPEN_CONFIG:
                ev = .openConfig
            case GHOSTTY_ACTION_PROMPT_TITLE:
                ev = .promptTitle(sid)
            case GHOSTTY_ACTION_FLOAT_WINDOW:
                let mode: FloatWindowMode
                switch action.action.float_window {
                case GHOSTTY_FLOAT_WINDOW_ON: mode = .on
                case GHOSTTY_FLOAT_WINDOW_OFF: mode = .off
                default: mode = .toggle
                }
                ev = .floatWindow(mode)
            case GHOSTTY_ACTION_COPY_TITLE_TO_CLIPBOARD:
                ev = .copyTitle(sid)
            case GHOSTTY_ACTION_START_SEARCH:
                let needle = action.action.start_search.needle.map { String(cString: $0) } ?? ""
                ev = .startSearch(sid, needle)
            case GHOSTTY_ACTION_END_SEARCH:
                ev = .endSearch(sid)
            case GHOSTTY_ACTION_SEARCH_TOTAL:
                ev = .searchTotal(sid, Int(action.action.search_total.total))
            case GHOSTTY_ACTION_SEARCH_SELECTED:
                ev = .searchSelected(sid, Int(action.action.search_selected.selected))
            case GHOSTTY_ACTION_SET_TITLE, GHOSTTY_ACTION_SET_TAB_TITLE:
                let p = action.tag == GHOSTTY_ACTION_SET_TITLE ? action.action.set_title.title : action.action.set_tab_title.title
                ev = .setTitle(sid, p.map { String(cString: $0) } ?? "")
            case GHOSTTY_ACTION_PWD:
                ev = .pwd(sid, action.action.pwd.pwd.map { String(cString: $0) })
            case GHOSTTY_ACTION_MOUSE_SHAPE:
                guard let shape = Self.mouseShape(action.action.mouse_shape) else { return true }
                ev = .mouseShape(sid, shape)
            case GHOSTTY_ACTION_MOUSE_VISIBILITY:
                ev = .mouseVisibility(action.action.mouse_visibility == GHOSTTY_MOUSE_VISIBLE)
            case GHOSTTY_ACTION_MOUSE_OVER_LINK:
                let v = action.action.mouse_over_link
                let url: String? = (v.len > 0 && v.url != nil)
                    ? String(decoding: UnsafeRawBufferPointer(start: v.url, count: v.len), as: UTF8.self) : nil
                ev = .mouseOverLink(sid, url)
            case GHOSTTY_ACTION_CELL_SIZE:
                let s = action.action.cell_size
                ev = .cellSize(sid, width: Double(s.width), height: Double(s.height))
            case GHOSTTY_ACTION_RING_BELL:
                ev = .ringBell(sid)
            case GHOSTTY_ACTION_OPEN_URL:
                let v = action.action.open_url
                guard let p = v.url else { return false }
                let s = String(decoding: UnsafeRawBufferPointer(start: p, count: Int(v.len)), as: UTF8.self)
                let kind: OpenURLKind
                switch v.kind {
                case GHOSTTY_ACTION_OPEN_URL_KIND_TEXT: kind = .text
                case GHOSTTY_ACTION_OPEN_URL_KIND_HTML: kind = .html
                case GHOSTTY_ACTION_OPEN_URL_KIND_OSC8: kind = .osc8
                default: kind = .unknown
                }
                ev = .openURL(sid, s, kind)
            case GHOSTTY_ACTION_DESKTOP_NOTIFICATION:
                let n = action.action.desktop_notification
                let title = n.title.map { String(cString: $0) } ?? ""
                let body = n.body.map { String(cString: $0) } ?? ""
                ev = .desktopNotification(title: title, body: body)
            case GHOSTTY_ACTION_COLOR_CHANGE:
                let c = action.action.color_change
                guard c.kind == GHOSTTY_ACTION_COLOR_KIND_BACKGROUND else { return true }
                ev = .backgroundColor(sid, GhosttyColor(r: c.r, g: c.g, b: c.b))
            case GHOSTTY_ACTION_CONFIG_CHANGE:
                return true
            case GHOSTTY_ACTION_SECURE_INPUT:
                let mode: SecureInputMode
                switch action.action.secure_input {
                case GHOSTTY_SECURE_INPUT_ON: mode = .on
                case GHOSTTY_SECURE_INPUT_OFF: mode = .off
                default: mode = .toggle
                }
                ev = .secureInput(sid, mode)
            case GHOSTTY_ACTION_RENDERER_HEALTH:
                guard action.action.renderer_health == GHOSTTY_RENDERER_HEALTH_UNHEALTHY else { return true }
                ev = .rendererUnhealthy
            case GHOSTTY_ACTION_SHOW_CHILD_EXITED:
                // Let the core render its own "process exited" message.
                return false
            case GHOSTTY_ACTION_RENDER, GHOSTTY_ACTION_INITIAL_SIZE, GHOSTTY_ACTION_SIZE_LIMIT,
                 GHOSTTY_ACTION_SCROLLBAR, GHOSTTY_ACTION_KEY_SEQUENCE, GHOSTTY_ACTION_KEY_TABLE,
                 GHOSTTY_ACTION_SELECTION_CHANGED, GHOSTTY_ACTION_QUIT_TIMER, GHOSTTY_ACTION_READONLY,
                 GHOSTTY_ACTION_PROGRESS_REPORT, GHOSTTY_ACTION_COMMAND_FINISHED:
                return true
            default:
                return false
            }

            app.enqueue(ev)
            return true
        }
    }

    private static func cbCloseSurface(_ ud: UnsafeMutableRawPointer?, processAlive: Bool) {
        GhosttyFFI.withCallback {
            guard let (sid, app) = app(forSurfaceUserdata: ud) else { return }
            app.enqueue(.closeSurface(sid, processAlive: processAlive))
        }
    }

    private static func cbReadClipboard(
        _ ud: UnsafeMutableRawPointer?,
        location: ghostty_clipboard_e,
        state: UnsafeMutableRawPointer?,
        mimes: UnsafePointer<UnsafePointer<CChar>?>?,
        mimesLen: Int,
        list: Bool
    ) -> ghostty_clipboard_read_result_e {
        GhosttyFFI.withCallback {
            guard let (sid, app) = app(forSurfaceUserdata: ud),
                  let loc = clipboardLocation(location) else {
                return GHOSTTY_CLIPBOARD_READ_UNSUPPORTED
            }
            var mimeList: [String] = []
            if let mimes {
                mimeList.reserveCapacity(mimesLen)
                for i in 0..<mimesLen {
                    if let p = mimes[i] { mimeList.append(String(cString: p)) }
                }
            }
            let tok = ClipboardRequestToken(state)
            app.enqueue(.clipboardRead(sid, loc, tok, mimes: mimeList, list: list), clipboardState: (sid, tok))
            return GHOSTTY_CLIPBOARD_READ_STARTED
        }
    }

    private static func cbConfirmReadClipboard(
        _ ud: UnsafeMutableRawPointer?,
        confirm: UnsafePointer<ghostty_clipboard_confirm_s>?,
        state: UnsafeMutableRawPointer?,
        request: ghostty_clipboard_request_e
    ) {
        GhosttyFFI.withCallback {
            guard let (sid, app) = app(forSurfaceUserdata: ud) else { return }
            let tok = ClipboardRequestToken(state)
            guard let confirm else {
                app.enqueue(.clipboardConfirmNil(sid, tok), clipboardState: (sid, tok))
                return
            }
            let c = confirm.pointee
            var reps: [ClipboardContent] = []
            if let contents = c.contents {
                for i in 0..<c.contents_len {
                    let item = contents[i]
                    guard let mime = item.mime else { continue }
                    let data = (item.len > 0 && item.data != nil) ? Data(bytes: item.data, count: item.len) : Data()
                    reps.append(ClipboardContent(mime: String(cString: mime), data: data))
                }
            }
            var avail: [String] = []
            if let a = c.available {
                for i in 0..<c.available_len {
                    if let p = a[i] { avail.append(String(cString: p)) }
                }
            }
            let kind: ClipboardRequestKind
            switch request {
            case GHOSTTY_CLIPBOARD_REQUEST_PASTE: kind = .paste
            case GHOSTTY_CLIPBOARD_REQUEST_OSC_52_READ: kind = .osc52Read
            case GHOSTTY_CLIPBOARD_REQUEST_OSC_52_WRITE: kind = .osc52Write
            case GHOSTTY_CLIPBOARD_REQUEST_KITTY_READ: kind = .kittyRead
            case GHOSTTY_CLIPBOARD_REQUEST_KITTY_WRITE: kind = .kittyWrite
            case GHOSTTY_CLIPBOARD_REQUEST_LIST: kind = .list
            default: kind = .other
            }
            app.enqueue(
                .clipboardConfirm(sid, contents: reps, available: avail, canRemember: c.can_remember, token: tok, kind: kind),
                clipboardState: (sid, tok)
            )
        }
    }

    private static func cbWriteClipboard(
        _ ud: UnsafeMutableRawPointer?,
        location: ghostty_clipboard_e,
        content: UnsafePointer<ghostty_clipboard_content_s>?,
        len: Int,
        confirm: Bool
    ) {
        GhosttyFFI.withCallback {
            guard let (sid, app) = app(forSurfaceUserdata: ud),
                  let loc = clipboardLocation(location),
                  let content, len > 0 else { return }
            var items: [ClipboardContent] = []
            for i in 0..<len {
                let c = content[i]
                guard let mime = c.mime, let data = c.data else { continue }
                items.append(ClipboardContent(mime: String(cString: mime), data: Data(bytes: data, count: c.len)))
            }
            guard !items.isEmpty else { return }
            app.enqueue(.clipboardWrite(sid, loc, contents: items, confirm: confirm))
        }
    }

    private static func clipboardLocation(_ loc: ghostty_clipboard_e) -> ClipboardLocation? {
        switch loc {
        case GHOSTTY_CLIPBOARD_STANDARD: return .standard
        case GHOSTTY_CLIPBOARD_SELECTION: return .selection
        default: return nil
        }
    }

    private static func mouseShape(_ shape: ghostty_action_mouse_shape_e) -> MouseShape? {
        switch shape {
        case GHOSTTY_MOUSE_SHAPE_DEFAULT: return .defaultArrow
        case GHOSTTY_MOUSE_SHAPE_TEXT: return .text
        case GHOSTTY_MOUSE_SHAPE_POINTER: return .pointer
        case GHOSTTY_MOUSE_SHAPE_GRAB: return .grab
        case GHOSTTY_MOUSE_SHAPE_GRABBING: return .grabbing
        case GHOSTTY_MOUSE_SHAPE_CROSSHAIR, GHOSTTY_MOUSE_SHAPE_CELL: return .crosshair
        case GHOSTTY_MOUSE_SHAPE_VERTICAL_TEXT: return .verticalText
        case GHOSTTY_MOUSE_SHAPE_CONTEXT_MENU: return .contextMenu
        case GHOSTTY_MOUSE_SHAPE_NOT_ALLOWED, GHOSTTY_MOUSE_SHAPE_NO_DROP: return .notAllowed
        case GHOSTTY_MOUSE_SHAPE_EW_RESIZE, GHOSTTY_MOUSE_SHAPE_COL_RESIZE: return .resizeEW
        case GHOSTTY_MOUSE_SHAPE_NS_RESIZE, GHOSTTY_MOUSE_SHAPE_ROW_RESIZE: return .resizeNS
        case GHOSTTY_MOUSE_SHAPE_W_RESIZE: return .resizeW
        case GHOSTTY_MOUSE_SHAPE_E_RESIZE: return .resizeE
        case GHOSTTY_MOUSE_SHAPE_N_RESIZE: return .resizeN
        case GHOSTTY_MOUSE_SHAPE_S_RESIZE: return .resizeS
        default: return nil
        }
    }
}

// MARK: - Surface wrapper

final class GhosttySurface {
    let id: SurfaceID
    private weak var app: GhosttyApp?
    private var raw: ghostty_surface_t?

    fileprivate init(id: SurfaceID, app: GhosttyApp, raw: ghostty_surface_t) {
        self.id = id
        self.app = app
        self.raw = raw
    }

    var isAlive: Bool { raw != nil }

    /// Denies any in-flight clipboard requests and frees the libghostty surface. Idempotent.
    func destroy() {
        guard let raw else { return }
        if let app {
            // Deny pending requests before freeing so libghostty releases their state.
            for token in app.takePendingClipboardStates(for: id) {
                GhosttyFFI.withFFI { ghostty_surface_deny_clipboard_request(raw, token.pointer) }
            }
        }
        self.raw = nil
        GhosttyApp.unregisterSurfaceApp(id)
        GhosttyFFI.withFFI { ghostty_surface_free(raw) }
    }

    deinit {
        if raw != nil {
            if Thread.isMainThread && !GhosttyFFI.isInCall {
                destroy()
            } else {
                let raw = self.raw!
                let id = self.id
                let app = self.app
                self.raw = nil
                DispatchQueue.main.async {
                    if let app {
                        for token in app.takePendingClipboardStates(for: id) {
                            GhosttyFFI.withFFI { ghostty_surface_deny_clipboard_request(raw, token.pointer) }
                        }
                    }
                    GhosttyApp.unregisterSurfaceApp(id)
                    GhosttyFFI.withFFI { ghostty_surface_free(raw) }
                }
            }
        }
    }

    var processExited: Bool {
        guard let raw else { return true }
        return GhosttyFFI.withFFI { ghostty_surface_process_exited(raw) }
    }

    var needsConfirmQuit: Bool {
        guard let raw else { return false }
        return GhosttyFFI.withFFI { ghostty_surface_needs_confirm_quit(raw) }
    }

    var gridSize: SurfaceGridSize {
        guard let raw else { return SurfaceGridSize(cols: 0, rows: 0) }
        return GhosttyFFI.withFFI {
            let s = ghostty_surface_size(raw)
            return SurfaceGridSize(cols: Int(s.columns), rows: Int(s.rows))
        }
    }

    func inheritedTabConfig() -> InheritedSurfaceConfig {
        guard let raw else { return InheritedSurfaceConfig(workingDirectory: nil, fontSize: 0) }
        return GhosttyFFI.withFFI {
            let inherited = ghostty_surface_inherited_config(raw, GHOSTTY_SURFACE_CONTEXT_TAB)
            let cwd = inherited.working_directory.map { String(cString: $0) }
            return InheritedSurfaceConfig(workingDirectory: cwd, fontSize: inherited.font_size)
        }
    }

    func setColorScheme(dark: Bool) {
        guard let raw else { return }
        let scheme = dark ? GHOSTTY_COLOR_SCHEME_DARK : GHOSTTY_COLOR_SCHEME_LIGHT
        GhosttyFFI.withFFI { ghostty_surface_set_color_scheme(raw, scheme) }
    }

    func setOcclusion(_ visible: Bool) {
        guard let raw else { return }
        GhosttyFFI.withFFI { ghostty_surface_set_occlusion(raw, visible) }
    }

    func setFocus(_ focused: Bool) {
        guard let raw else { return }
        GhosttyFFI.withFFI { ghostty_surface_set_focus(raw, focused) }
    }

    func setSize(width: UInt32, height: UInt32) {
        guard let raw else { return }
        GhosttyFFI.withFFI { ghostty_surface_set_size(raw, width, height) }
    }

    func setContentScale(x: Double, y: Double) {
        guard let raw else { return }
        GhosttyFFI.withFFI { ghostty_surface_set_content_scale(raw, x, y) }
    }

    func setDisplayID(_ displayID: UInt32) {
        guard let raw else { return }
        GhosttyFFI.withFFI { ghostty_surface_set_display_id(raw, displayID) }
    }

    @discardableResult
    func bindingAction(_ action: String) -> Bool {
        guard let raw else { return false }
        return GhosttyFFI.withFFI {
            ghostty_surface_binding_action(raw, action, UInt(action.utf8.count))
        }
    }

    func requestClose() {
        guard let raw else { return }
        GhosttyFFI.withFFI { ghostty_surface_request_close(raw) }
    }

    @discardableResult
    func mouseButton(state: MouseButtonState, button: MouseButton, mods: GhosttyMods) -> Bool {
        guard let raw else { return false }
        let cState = state == .press ? GHOSTTY_MOUSE_PRESS : GHOSTTY_MOUSE_RELEASE
        let cButton = Self.cMouseButton(button)
        let cMods = Self.cMods(mods)
        return GhosttyFFI.withFFI {
            ghostty_surface_mouse_button(raw, cState, cButton, cMods)
        }
    }

    func mousePos(x: Double, y: Double, mods: GhosttyMods) {
        guard let raw else { return }
        let cMods = Self.cMods(mods)
        GhosttyFFI.withFFI { ghostty_surface_mouse_pos(raw, x, y, cMods) }
    }

    func mouseScroll(x: Double, y: Double, precise: Bool, momentum: ScrollMomentum) {
        guard let raw else { return }
        var m: Int32 = precise ? 1 : 0
        m |= Int32(momentum.rawValue) << 1
        GhosttyFFI.withFFI { ghostty_surface_mouse_scroll(raw, x, y, m) }
    }

    func mousePressure(stage: UInt32, pressure: Double) {
        guard let raw else { return }
        GhosttyFFI.withFFI { ghostty_surface_mouse_pressure(raw, stage, pressure) }
    }

    func keyTranslationMods(_ mods: GhosttyMods) -> GhosttyMods {
        guard let raw else { return mods }
        let cMods = Self.cMods(mods)
        return GhosttyFFI.withFFI {
            Self.swiftMods(ghostty_surface_key_translation_mods(raw, cMods))
        }
    }

    @discardableResult
    func key(_ event: GhosttyKeyEvent) -> Bool {
        guard let raw else { return false }
        var kev = Self.cKeyEvent(event)
        return GhosttyFFI.withFFI {
            if let text = event.text {
                return text.withCString { p in
                    kev.text = p
                    return ghostty_surface_key(raw, kev)
                }
            }
            return ghostty_surface_key(raw, kev)
        }
    }

    func keyIsBinding(_ event: GhosttyKeyEvent) -> Bool {
        guard let raw else { return false }
        var kev = Self.cKeyEvent(event)
        var flags = ghostty_binding_flags_e(0)
        return GhosttyFFI.withFFI {
            (event.text ?? "").withCString { p in
                kev.text = p
                return ghostty_surface_key_is_binding(raw, kev, &flags)
            }
        }
    }

    func text(_ s: String) {
        guard let raw else { return }
        GhosttyFFI.withFFI {
            s.withCString { p in ghostty_surface_text(raw, p, UInt(s.utf8.count)) }
        }
    }

    func pasteText(_ s: String) {
        guard let raw else { return }
        GhosttyFFI.withFFI {
            s.withCString { p in ghostty_surface_paste_text(raw, p, UInt(s.utf8.count)) }
        }
    }

    func preedit(_ s: String?) {
        guard let raw else { return }
        GhosttyFFI.withFFI {
            if let s, !s.isEmpty {
                s.withCString { p in ghostty_surface_preedit(raw, p, UInt(s.utf8.count)) }
            } else {
                ghostty_surface_preedit(raw, nil, 0)
            }
        }
    }

    func readSelection() -> SurfaceTextResult? {
        guard let raw else { return nil }
        return GhosttyFFI.withFFI {
            var text = ghostty_text_s()
            guard ghostty_surface_read_selection(raw, &text) else { return nil }
            defer { ghostty_surface_free_text(raw, &text) }
            return SurfaceTextResult(
                text: text.text.map { String(cString: $0) } ?? "",
                tlPxX: text.tl_px_x,
                tlPxY: text.tl_px_y,
                offsetStart: Int(text.offset_start),
                offsetLen: Int(text.offset_len)
            )
        }
    }

    func readViewportText() -> String {
        guard let raw else { return "" }
        return GhosttyFFI.withFFI {
            var text = ghostty_text_s()
            let sel = ghostty_selection_s(
                top_left: ghostty_point_s(tag: GHOSTTY_POINT_VIEWPORT, coord: GHOSTTY_POINT_COORD_TOP_LEFT, x: 0, y: 0),
                bottom_right: ghostty_point_s(tag: GHOSTTY_POINT_VIEWPORT, coord: GHOSTTY_POINT_COORD_BOTTOM_RIGHT, x: 0, y: 0),
                rectangle: false
            )
            guard ghostty_surface_read_text(raw, sel, &text) else { return "" }
            defer { ghostty_surface_free_text(raw, &text) }
            return text.text.map { String(cString: $0) } ?? ""
        }
    }

    func quicklookFont() -> CTFont? {
        guard let raw else { return nil }
        return GhosttyFFI.withFFI {
            guard let ptr = ghostty_surface_quicklook_font(raw) else { return nil }
            return Unmanaged<CTFont>.fromOpaque(ptr).takeRetainedValue()
        }
    }

    func quicklookWord() -> SurfaceTextResult? {
        guard let raw else { return nil }
        return GhosttyFFI.withFFI {
            var text = ghostty_text_s()
            guard ghostty_surface_quicklook_word(raw, &text) else { return nil }
            defer { ghostty_surface_free_text(raw, &text) }
            guard text.text_len > 0 else { return nil }
            return SurfaceTextResult(
                text: text.text.map { String(cString: $0) } ?? "",
                tlPxX: text.tl_px_x,
                tlPxY: text.tl_px_y,
                offsetStart: Int(text.offset_start),
                offsetLen: Int(text.offset_len)
            )
        }
    }

    func imePoint() -> SurfaceIMEPoint {
        guard let raw else { return SurfaceIMEPoint(x: 0, y: 0, width: 0, height: 0) }
        return GhosttyFFI.withFFI {
            var x: Double = 0, y: Double = 0, w: Double = 0, h: Double = 0
            ghostty_surface_ime_point(raw, &x, &y, &w, &h)
            return SurfaceIMEPoint(x: x, y: y, width: w, height: h)
        }
    }

    func completeClipboard(
        contents: [ClipboardContent],
        available: [String],
        token: ClipboardRequestToken,
        confirmed: Bool = false,
        remember: Bool = false
    ) {
        guard let raw, let app, app.consumeClipboardState(token, for: id) else { return }
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
        GhosttyFFI.withFFI {
            cContents.withUnsafeBufferPointer { cb in
                cAvail.withUnsafeBufferPointer { ab in
                    var complete = ghostty_clipboard_complete_s(
                        contents: cb.baseAddress, contents_len: cb.count,
                        available: ab.baseAddress, available_len: ab.count,
                        confirmed: confirmed, remember: remember
                    )
                    ghostty_surface_complete_clipboard_request(raw, &complete, token.pointer)
                }
            }
        }
    }

    func denyClipboard(token: ClipboardRequestToken) {
        guard let raw, let app, app.consumeClipboardState(token, for: id) else { return }
        GhosttyFFI.withFFI { ghostty_surface_deny_clipboard_request(raw, token.pointer) }
    }

    // MARK: C enum conversions

    private static func cMods(_ mods: GhosttyMods) -> ghostty_input_mods_e {
        var m: UInt32 = GHOSTTY_MODS_NONE.rawValue
        if mods.contains(.shift) { m |= GHOSTTY_MODS_SHIFT.rawValue }
        if mods.contains(.ctrl) { m |= GHOSTTY_MODS_CTRL.rawValue }
        if mods.contains(.alt) { m |= GHOSTTY_MODS_ALT.rawValue }
        if mods.contains(.superMod) { m |= GHOSTTY_MODS_SUPER.rawValue }
        if mods.contains(.caps) { m |= GHOSTTY_MODS_CAPS.rawValue }
        if mods.contains(.num) { m |= GHOSTTY_MODS_NUM.rawValue }
        if mods.contains(.shiftRight) { m |= GHOSTTY_MODS_SHIFT_RIGHT.rawValue }
        if mods.contains(.ctrlRight) { m |= GHOSTTY_MODS_CTRL_RIGHT.rawValue }
        if mods.contains(.altRight) { m |= GHOSTTY_MODS_ALT_RIGHT.rawValue }
        if mods.contains(.superRight) { m |= GHOSTTY_MODS_SUPER_RIGHT.rawValue }
        return ghostty_input_mods_e(m)
    }

    private static func swiftMods(_ m: ghostty_input_mods_e) -> GhosttyMods {
        var f: GhosttyMods = []
        if m.rawValue & GHOSTTY_MODS_SHIFT.rawValue != 0 { f.insert(.shift) }
        if m.rawValue & GHOSTTY_MODS_CTRL.rawValue != 0 { f.insert(.ctrl) }
        if m.rawValue & GHOSTTY_MODS_ALT.rawValue != 0 { f.insert(.alt) }
        if m.rawValue & GHOSTTY_MODS_SUPER.rawValue != 0 { f.insert(.superMod) }
        if m.rawValue & GHOSTTY_MODS_CAPS.rawValue != 0 { f.insert(.caps) }
        if m.rawValue & GHOSTTY_MODS_NUM.rawValue != 0 { f.insert(.num) }
        if m.rawValue & GHOSTTY_MODS_SHIFT_RIGHT.rawValue != 0 { f.insert(.shiftRight) }
        if m.rawValue & GHOSTTY_MODS_CTRL_RIGHT.rawValue != 0 { f.insert(.ctrlRight) }
        if m.rawValue & GHOSTTY_MODS_ALT_RIGHT.rawValue != 0 { f.insert(.altRight) }
        if m.rawValue & GHOSTTY_MODS_SUPER_RIGHT.rawValue != 0 { f.insert(.superRight) }
        return f
    }

    private static func cKeyEvent(_ e: GhosttyKeyEvent) -> ghostty_input_key_s {
        var k = ghostty_input_key_s()
        switch e.action {
        case .release: k.action = GHOSTTY_ACTION_RELEASE
        case .press: k.action = GHOSTTY_ACTION_PRESS
        case .repeatKey: k.action = GHOSTTY_ACTION_REPEAT
        }
        k.mods = cMods(e.mods)
        k.consumed_mods = cMods(e.consumedMods)
        k.keycode = e.keycode
        k.text = nil
        k.unshifted_codepoint = e.unshiftedCodepoint
        k.composing = e.composing
        return k
    }

    private static func cMouseButton(_ b: MouseButton) -> ghostty_input_mouse_button_e {
        switch b {
        case .unknown: return GHOSTTY_MOUSE_UNKNOWN
        case .left: return GHOSTTY_MOUSE_LEFT
        case .right: return GHOSTTY_MOUSE_RIGHT
        case .middle: return GHOSTTY_MOUSE_MIDDLE
        case .four: return GHOSTTY_MOUSE_FOUR
        case .five: return GHOSTTY_MOUSE_FIVE
        case .six: return GHOSTTY_MOUSE_SIX
        case .seven: return GHOSTTY_MOUSE_SEVEN
        case .eight: return GHOSTTY_MOUSE_EIGHT
        case .nine: return GHOSTTY_MOUSE_NINE
        case .ten: return GHOSTTY_MOUSE_TEN
        case .eleven: return GHOSTTY_MOUSE_ELEVEN
        }
    }
}

// MARK: - C string helpers

private func withOptionalCString<R>(_ s: String?, _ body: (UnsafePointer<CChar>?) -> R) -> R {
    guard let s, !s.isEmpty else { return body(nil) }
    return s.withCString { body($0) }
}

private func withCStringPairs<R>(_ pairs: [(String, String)],
                                 _ body: ([(UnsafePointer<CChar>, UnsafePointer<CChar>)]) -> R) -> R {
    var owned: [UnsafeMutablePointer<CChar>] = []
    defer { owned.forEach { free($0) } }
    var out: [(UnsafePointer<CChar>, UnsafePointer<CChar>)] = []
    for (k, v) in pairs {
        guard let ck = strdup(k), let cv = strdup(v) else { continue }
        owned.append(ck); owned.append(cv)
        out.append((UnsafePointer(ck), UnsafePointer(cv)))
    }
    return body(out)
}
