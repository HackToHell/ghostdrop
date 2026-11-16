import AppKit
import Carbon.HIToolbox
import GhosttyKit

/// Owner of a set of surfaces (the drop-down controller).
protocol SurfaceOwner: AnyObject {
    func surfaceRequestedClose(_ view: SurfaceView, needsConfirm: Bool)
    func surfaceTitleDidChange(_ view: SurfaceView)
    func surfaceDidRingBell(_ view: SurfaceView)
    func surfaceBackgroundDidChange(_ view: SurfaceView)
    func surfaceSwiped(_ view: SurfaceView, next: Bool)
}

/// An NSView hosting one libghostty surface (one terminal). libghostty attaches
/// its own Metal (IOSurface) layer to this view and renders on its own thread.
final class SurfaceView: NSView, NSTextInputClient {
    private(set) var surface: ghostty_surface_t?
    weak var owner: SurfaceOwner?
    let profile: Profile

    // State reported by the terminal.
    private(set) var titleFromTerminal = ""
    var titleOverride: String? { didSet { owner?.surfaceTitleDidChange(self) } }
    var pwd: String?
    var cellSize: NSSize = .zero
    var hoverURL: String? { didSet { toolTip = hoverURL } }
    var backgroundColorOverride: NSColor? { didSet { owner?.surfaceBackgroundDidChange(self) } }
    private(set) var bellActive = false
    private var titleTimer: Timer?
    /// At a password prompt: secure keyboard entry while this surface is focused.
    private(set) var passwordInput = false

    var title: String {
        if let titleOverride, !titleOverride.isEmpty { return titleOverride }
        if !titleFromTerminal.isEmpty { return titleFromTerminal }
        return profile.title ?? profile.name
    }

    // Input state.
    private var markedText = NSMutableAttributedString()
    private var keyTextAccumulator: [String]?
    private var lastPerformKeyEvent: TimeInterval?
    private var suppressNextLeftMouseUp = false
    private var prevPressureStage = 0
    private var leadSurrogate: UInt16?
    private(set) var focused = false
    private var contentSize: NSSize?
    private var cursor: NSCursor = .iBeam

    override var acceptsFirstResponder: Bool { true }
    override var isOpaque: Bool { false }

    init(runtime: GhosttyRuntime, profile: Profile, workingDirectory: String?, fontSize: Float = 0,
         context: ghostty_surface_context_e = GHOSTTY_SURFACE_CONTEXT_TAB) {
        self.profile = profile
        super.init(frame: NSRect(x: 0, y: 0, width: 800, height: 600))

        guard let app = runtime.app else { return }
        var cfg = ghostty_surface_config_new()
        cfg.userdata = Unmanaged.passUnretained(self).toOpaque()
        cfg.platform_tag = GHOSTTY_PLATFORM_MACOS
        cfg.platform = ghostty_platform_u(macos: ghostty_platform_macos_s(nsview: Unmanaged.passUnretained(self).toOpaque()))
        cfg.scale_factor = Double(NSScreen.main?.backingScaleFactor ?? 2)
        cfg.font_size = fontSize
        cfg.wait_after_command = profile.waitAfterCommand
        cfg.context = context

        let wd = profile.workingDirectory ?? workingDirectory
        let env = profile.env.map { ($0.key, $0.value) }
        surface = withOptionalCString(wd) { cwd in
            withOptionalCString(profile.effectiveCommand) { cmd in
                withCStringPairs(env) { pairs in
                    var vars = pairs.map { ghostty_env_var_s(key: $0.0, value: $0.1) }
                    return vars.withUnsafeMutableBufferPointer { buf in
                        cfg.working_directory = cwd
                        cfg.command = cmd
                        cfg.env_vars = buf.count > 0 ? buf.baseAddress : nil
                        cfg.env_var_count = buf.count
                        return ghostty_surface_new(app, &cfg)
                    }
                }
            }
        }
        updateTrackingAreas()
        registerForDraggedTypes([.fileURL, .URL, .string])
    }

    required init?(coder: NSCoder) { fatalError("not supported") }

    /// Frees the libghostty surface (kills the child process). Idempotent.
    func destroy() {
        titleTimer?.invalidate()
        if passwordInput {
            passwordInput = false
            SecureInput.shared.removeScoped(ObjectIdentifier(self))
        }
        guard let s = surface else { return }
        surface = nil
        ghostty_surface_free(s)
    }

    deinit { destroy() }

    var processExited: Bool { surface.map { ghostty_surface_process_exited($0) } ?? true }
    var needsConfirmQuit: Bool { surface.map { ghostty_surface_needs_confirm_quit($0) } ?? false }

    // MARK: - Titles & bell

    func setTitleFromTerminal(_ t: String) {
        // Coalesce rapid title updates (e.g. shells setting it per prompt).
        titleTimer?.invalidate()
        titleTimer = Timer.scheduledTimer(withTimeInterval: 0.05, repeats: false) { [weak self] _ in
            guard let self else { return }
            self.titleFromTerminal = t
            self.owner?.surfaceTitleDidChange(self)
        }
    }

    func ringBell() {
        if focused && window?.isKeyWindow == true { return }
        bellActive = true
        owner?.surfaceDidRingBell(self)
    }

    func clearBell() {
        guard bellActive else { return }
        bellActive = false
        owner?.surfaceTitleDidChange(self)
    }

    func bindingAction(_ action: String) {
        guard let surface else { return }
        _ = ghostty_surface_binding_action(surface, action, UInt(action.utf8.count))
    }

    func setPasswordInput(_ mode: ghostty_action_secure_input_e) {
        guard surface != nil else { return }
        switch mode {
        case GHOSTTY_SECURE_INPUT_ON: passwordInput = true
        case GHOSTTY_SECURE_INPUT_OFF: passwordInput = false
        default: passwordInput.toggle()
        }
        if passwordInput {
            SecureInput.shared.setScoped(ObjectIdentifier(self), focused: focused)
        } else {
            SecureInput.shared.removeScoped(ObjectIdentifier(self))
        }
    }

    func setOcclusion(visible: Bool) {
        guard let surface else { return }
        ghostty_surface_set_occlusion(surface, visible)
    }

    // MARK: - Focus

    func focusDidChange(_ f: Bool) {
        guard let surface, focused != f else { return }
        focused = f
        if !f { suppressNextLeftMouseUp = false }
        ghostty_surface_set_focus(surface, f)
        if passwordInput { SecureInput.shared.setScoped(ObjectIdentifier(self), focused: f) }
        if f { clearBell() }
    }

    override func becomeFirstResponder() -> Bool {
        let r = super.becomeFirstResponder()
        if r { focusDidChange(window?.isKeyWindow ?? false) }
        return r
    }

    override func resignFirstResponder() -> Bool {
        let r = super.resignFirstResponder()
        if r { focusDidChange(false) }
        return r
    }

    // MARK: - Geometry

    override func setFrameSize(_ newSize: NSSize) {
        super.setFrameSize(newSize)
        sizeDidChange(newSize)
    }

    private func sizeDidChange(_ size: NSSize) {
        contentSize = size
        guard let surface, size.width > 0, size.height > 0 else { return }
        let scaled = convertToBacking(size)
        ghostty_surface_set_size(surface, UInt32(scaled.width), UInt32(scaled.height))
    }

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        viewDidChangeBackingProperties()
        updateDisplayID()
    }

    func updateDisplayID() {
        guard let surface, let screen = window?.screen,
              let id = screen.deviceDescription[NSDeviceDescriptionKey("NSScreenNumber")] as? UInt32 else { return }
        ghostty_surface_set_display_id(surface, id)
    }

    override func viewDidChangeBackingProperties() {
        super.viewDidChangeBackingProperties()
        if let window {
            CATransaction.begin()
            CATransaction.setDisableActions(true)
            layer?.contentsScale = window.backingScaleFactor
            CATransaction.commit()
        }
        guard let surface, frame.width > 0, frame.height > 0 else { return }
        let fb = convertToBacking(frame)
        ghostty_surface_set_content_scale(surface, fb.width / frame.width, fb.height / frame.height)
        let scaled = convertToBacking(contentSize ?? frame.size)
        ghostty_surface_set_size(surface, UInt32(scaled.width), UInt32(scaled.height))
    }

    override func updateTrackingAreas() {
        trackingAreas.forEach { removeTrackingArea($0) }
        addTrackingArea(NSTrackingArea(
            rect: frame,
            options: [.mouseEnteredAndExited, .mouseMoved, .inVisibleRect, .activeAlways, .cursorUpdate],
            owner: self, userInfo: nil))
        super.updateTrackingAreas()
    }

    // MARK: - Cursor

    func setCursorShape(_ shape: ghostty_action_mouse_shape_e) {
        let c: NSCursor
        switch shape {
        case GHOSTTY_MOUSE_SHAPE_DEFAULT: c = .arrow
        case GHOSTTY_MOUSE_SHAPE_TEXT: c = .iBeam
        case GHOSTTY_MOUSE_SHAPE_POINTER: c = .pointingHand
        case GHOSTTY_MOUSE_SHAPE_GRAB: c = .openHand
        case GHOSTTY_MOUSE_SHAPE_GRABBING: c = .closedHand
        case GHOSTTY_MOUSE_SHAPE_CROSSHAIR, GHOSTTY_MOUSE_SHAPE_CELL: c = .crosshair
        case GHOSTTY_MOUSE_SHAPE_VERTICAL_TEXT: c = .iBeamCursorForVerticalLayout
        case GHOSTTY_MOUSE_SHAPE_CONTEXT_MENU: c = .contextualMenu
        case GHOSTTY_MOUSE_SHAPE_NOT_ALLOWED, GHOSTTY_MOUSE_SHAPE_NO_DROP: c = .operationNotAllowed
        case GHOSTTY_MOUSE_SHAPE_EW_RESIZE, GHOSTTY_MOUSE_SHAPE_COL_RESIZE: c = .resizeLeftRight
        case GHOSTTY_MOUSE_SHAPE_NS_RESIZE, GHOSTTY_MOUSE_SHAPE_ROW_RESIZE: c = .resizeUpDown
        case GHOSTTY_MOUSE_SHAPE_W_RESIZE: c = .resizeLeft
        case GHOSTTY_MOUSE_SHAPE_E_RESIZE: c = .resizeRight
        case GHOSTTY_MOUSE_SHAPE_N_RESIZE: c = .resizeUp
        case GHOSTTY_MOUSE_SHAPE_S_RESIZE: c = .resizeDown
        default: return
        }
        cursor = c
        window?.invalidateCursorRects(for: self)
        if let w = window, w.isKeyWindow,
           let content = w.contentView,
           content.hitTest(content.convert(w.mouseLocationOutsideOfEventStream, from: nil)) === self {
            c.set()
        }
    }

    override func resetCursorRects() {
        addCursorRect(bounds, cursor: cursor)
    }

    override func cursorUpdate(with event: NSEvent) {
        cursor.set()
    }

    // MARK: - Mouse

    private func mods(_ flags: NSEvent.ModifierFlags) -> ghostty_input_mods_e { GhosttyInput.mods(flags) }

    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { false }

    /// Focus-transfer clicks shouldn't be reported to the terminal.
    func handleFocusClick(_ event: NSEvent) -> Bool {
        guard let window, window.firstResponder !== self else { return false }
        window.makeFirstResponder(self)
        if NSApp.isActive && window.isKeyWindow {
            suppressNextLeftMouseUp = true
            return true
        }
        return false
    }

    override func mouseDown(with event: NSEvent) {
        if handleFocusClick(event) { return }
        guard let surface else { return }
        ghostty_surface_mouse_button(surface, GHOSTTY_MOUSE_PRESS, GHOSTTY_MOUSE_LEFT, mods(event.modifierFlags))
    }

    override func mouseUp(with event: NSEvent) {
        if suppressNextLeftMouseUp {
            suppressNextLeftMouseUp = false
            return
        }
        prevPressureStage = 0
        guard let surface else { return }
        ghostty_surface_mouse_button(surface, GHOSTTY_MOUSE_RELEASE, GHOSTTY_MOUSE_LEFT, mods(event.modifierFlags))
        ghostty_surface_mouse_pressure(surface, 0, 0)
    }

    override func rightMouseDown(with event: NSEvent) {
        guard let surface else { return super.rightMouseDown(with: event) }
        if !ghostty_surface_mouse_button(surface, GHOSTTY_MOUSE_PRESS, GHOSTTY_MOUSE_RIGHT, mods(event.modifierFlags)) {
            super.rightMouseDown(with: event)
        }
    }

    override func rightMouseUp(with event: NSEvent) {
        guard let surface else { return super.rightMouseUp(with: event) }
        if !ghostty_surface_mouse_button(surface, GHOSTTY_MOUSE_RELEASE, GHOSTTY_MOUSE_RIGHT, mods(event.modifierFlags)) {
            super.rightMouseUp(with: event)
        }
    }

    override func otherMouseDown(with event: NSEvent) {
        guard let surface else { return }
        ghostty_surface_mouse_button(surface, GHOSTTY_MOUSE_PRESS, GhosttyInput.button(event.buttonNumber), mods(event.modifierFlags))
    }

    override func otherMouseUp(with event: NSEvent) {
        guard let surface else { return }
        ghostty_surface_mouse_button(surface, GHOSTTY_MOUSE_RELEASE, GhosttyInput.button(event.buttonNumber), mods(event.modifierFlags))
    }

    override func mouseEntered(with event: NSEvent) {
        super.mouseEntered(with: event)
        sendMousePos(event)
    }

    override func mouseExited(with event: NSEvent) {
        guard let surface else { return }
        if NSEvent.pressedMouseButtons != 0 { return }
        ghostty_surface_mouse_pos(surface, -1, -1, mods(event.modifierFlags))
    }

    override func mouseMoved(with event: NSEvent) { sendMousePos(event) }
    override func mouseDragged(with event: NSEvent) { sendMousePos(event) }
    override func rightMouseDragged(with event: NSEvent) { sendMousePos(event) }
    override func otherMouseDragged(with event: NSEvent) { sendMousePos(event) }

    private func sendMousePos(_ event: NSEvent) {
        guard let surface else { return }
        let p = convert(event.locationInWindow, from: nil)
        ghostty_surface_mouse_pos(surface, p.x, frame.height - p.y, mods(event.modifierFlags))
    }

    override func scrollWheel(with event: NSEvent) {
        guard let surface else { return }
        var x = event.scrollingDeltaX
        var y = event.scrollingDeltaY
        let precise = event.hasPreciseScrollingDeltas
        if precise { x *= 2; y *= 2 }
        var m: Int32 = precise ? 1 : 0
        m |= Int32(GhosttyInput.momentum(event.momentumPhase)) << 1
        ghostty_surface_mouse_scroll(surface, x, y, m)
    }

    /// Three-finger swipe left/right switches tabs (like iTerm2).
    override func swipe(with event: NSEvent) {
        if event.deltaX != 0 { owner?.surfaceSwiped(self, next: event.deltaX < 0) }
    }

    override func pressureChange(with event: NSEvent) {
        guard let surface else { return }
        ghostty_surface_mouse_pressure(surface, UInt32(event.stage), Double(event.pressure))
        guard prevPressureStage < 2 else { return }
        prevPressureStage = event.stage
        guard event.stage == 2 else { return }
        if UserDefaults.standard.bool(forKey: "com.apple.trackpad.forceClick") { quickLook(with: event) }
    }

    // MARK: - Keyboard

    override func keyDown(with event: NSEvent) {
        guard let surface else {
            interpretKeyEvents([event])
            return
        }
        clearBell()

        // Translate mods (e.g. macos-option-as-alt) for text generation.
        let translated = GhosttyInput.flags(ghostty_surface_key_translation_mods(surface, mods(event.modifierFlags)))
        var translationMods = event.modifierFlags
        for flag in [NSEvent.ModifierFlags.shift, .control, .option, .command] {
            if translated.contains(flag) { translationMods.insert(flag) } else { translationMods.remove(flag) }
        }
        let translationEvent: NSEvent
        if translationMods == event.modifierFlags {
            translationEvent = event
        } else {
            translationEvent = NSEvent.keyEvent(
                with: event.type, location: event.locationInWindow, modifierFlags: translationMods,
                timestamp: event.timestamp, windowNumber: event.windowNumber, context: nil,
                characters: event.characters(byApplyingModifiers: translationMods) ?? "",
                charactersIgnoringModifiers: event.charactersIgnoringModifiers ?? "",
                isARepeat: event.isARepeat, keyCode: event.keyCode) ?? event
        }

        let action = event.isARepeat ? GHOSTTY_ACTION_REPEAT : GHOSTTY_ACTION_PRESS
        keyTextAccumulator = []
        defer { keyTextAccumulator = nil }

        let markedBefore = markedText.length > 0
        let layoutBefore: String? = markedBefore ? nil : GhosttyInput.keyboardLayoutID()
        lastPerformKeyEvent = nil

        interpretKeyEvents([translationEvent])

        // An input method switched the layout: swallow.
        if !markedBefore && layoutBefore != GhosttyInput.keyboardLayoutID() { return }

        syncPreedit(clearIfNeeded: markedBefore)
        let composing = markedText.length > 0 || markedBefore

        if markedBefore, let list = keyTextAccumulator, !list.isEmpty {
            for text in list where !Self.suppressComposingControl(text, composing: composing) {
                _ = committedText(action, text)
            }
            if Self.shouldReplayCommittedPreeditKey(translationEvent) {
                _ = keyAction(action, event: event, translationEvent: translationEvent, composing: false)
            }
            return
        }

        if let list = keyTextAccumulator, !list.isEmpty {
            for text in list where !Self.suppressComposingControl(text, composing: composing) {
                _ = keyAction(action, event: event, translationEvent: translationEvent, text: text)
            }
        } else {
            if Self.suppressComposingControl(event.characters, composing: composing) { return }
            _ = keyAction(action, event: event, translationEvent: translationEvent,
                          text: GhosttyInput.characters(translationEvent), composing: composing)
        }
    }

    override func keyUp(with event: NSEvent) {
        _ = keyAction(GHOSTTY_ACTION_RELEASE, event: event)
    }

    override func performKeyEquivalent(with event: NSEvent) -> Bool {
        guard event.type == .keyDown, focused, let surface else { return false }

        // Is it a Ghostty keybinding? Then send it through keyDown.
        var kev = GhosttyInput.keyEvent(event, GHOSTTY_ACTION_PRESS)
        var flags = ghostty_binding_flags_e(0)
        let isBinding = (event.characters ?? "").withCString { p -> Bool in
            kev.text = p
            return ghostty_surface_key_is_binding(surface, kev, &flags)
        }
        if isBinding {
            keyDown(with: event)
            return true
        }

        let equivalent: String
        switch event.charactersIgnoringModifiers {
        case "\r":
            if !event.modifierFlags.contains(.control) { return false }
            equivalent = "\r"
        case "/":
            if !event.modifierFlags.contains(.control) ||
                !event.modifierFlags.isDisjoint(with: [.shift, .command, .option]) { return false }
            equivalent = "_"
        default:
            if event.timestamp == 0 { return false }
            if !event.modifierFlags.contains(.command) && !event.modifierFlags.contains(.control) {
                lastPerformKeyEvent = nil
                return false
            }
            if let last = lastPerformKeyEvent {
                lastPerformKeyEvent = nil
                if last == event.timestamp {
                    equivalent = event.characters ?? ""
                    break
                }
            }
            lastPerformKeyEvent = event.timestamp
            return false
        }

        guard let final = NSEvent.keyEvent(
            with: .keyDown, location: event.locationInWindow, modifierFlags: event.modifierFlags,
            timestamp: event.timestamp, windowNumber: event.windowNumber, context: nil,
            characters: equivalent, charactersIgnoringModifiers: equivalent,
            isARepeat: event.isARepeat, keyCode: event.keyCode) else { return false }
        keyDown(with: final)
        return true
    }

    override func flagsChanged(with event: NSEvent) {
        let mod: UInt32
        switch event.keyCode {
        case 0x39: mod = GHOSTTY_MODS_CAPS.rawValue
        case 0x38, 0x3C: mod = GHOSTTY_MODS_SHIFT.rawValue
        case 0x3B, 0x3E: mod = GHOSTTY_MODS_CTRL.rawValue
        case 0x3A, 0x3D: mod = GHOSTTY_MODS_ALT.rawValue
        case 0x37, 0x36: mod = GHOSTTY_MODS_SUPER.rawValue
        default: return
        }
        if hasMarkedText() { return }
        let m = mods(event.modifierFlags)
        var action = GHOSTTY_ACTION_RELEASE
        if m.rawValue & mod != 0 {
            let raw = event.modifierFlags.rawValue
            let sidePressed: Bool
            switch event.keyCode {
            case 0x3C: sidePressed = raw & UInt(NX_DEVICERSHIFTKEYMASK) != 0
            case 0x3E: sidePressed = raw & UInt(NX_DEVICERCTLKEYMASK) != 0
            case 0x3D: sidePressed = raw & UInt(NX_DEVICERALTKEYMASK) != 0
            case 0x36: sidePressed = raw & UInt(NX_DEVICERCMDKEYMASK) != 0
            default: sidePressed = true
            }
            if sidePressed { action = GHOSTTY_ACTION_PRESS }
        }
        _ = keyAction(action, event: event)
    }

    private func keyAction(_ action: ghostty_input_action_e, event: NSEvent, translationEvent: NSEvent? = nil,
                           text: String? = nil, composing: Bool = false) -> Bool {
        guard let surface else { return false }
        var kev = GhosttyInput.keyEvent(event, action, translationMods: translationEvent?.modifierFlags)
        kev.composing = composing
        if let text, !text.isEmpty, let first = text.unicodeScalars.first, first.value >= 0x20, first.value != 0x7F {
            return text.withCString { p in
                kev.text = p
                return ghostty_surface_key(surface, kev)
            }
        }
        return ghostty_surface_key(surface, kev)
    }

    private func committedText(_ action: ghostty_input_action_e, _ text: String) -> Bool {
        guard let surface else { return false }
        var kev = ghostty_input_key_s()
        kev.action = action
        kev.mods = GHOSTTY_MODS_NONE
        kev.consumed_mods = GHOSTTY_MODS_NONE
        return text.withCString { p in
            kev.text = p
            return ghostty_surface_key(surface, kev)
        }
    }

    private static func shouldReplayCommittedPreeditKey(_ event: NSEvent) -> Bool {
        switch event.keyCode {
        case 125, 124, 126: return true // down, right, up
        case 123: return !event.modifierFlags.isDisjoint(with: [.shift, .control, .option, .command])
        default: return false
        }
    }

    private static func suppressComposingControl(_ text: String?, composing: Bool) -> Bool {
        guard composing, let text, text.unicodeScalars.count == 1, let s = text.unicodeScalars.first else { return false }
        return s.value < 0x20
    }

    // MARK: - NSTextInputClient

    func hasMarkedText() -> Bool { markedText.length > 0 }

    func markedRange() -> NSRange {
        markedText.length > 0 ? NSRange(location: 0, length: markedText.length) : NSRange()
    }

    func selectedRange() -> NSRange {
        guard let surface else { return NSRange() }
        var text = ghostty_text_s()
        guard ghostty_surface_read_selection(surface, &text) else { return NSRange() }
        defer { ghostty_surface_free_text(surface, &text) }
        return NSRange(location: Int(text.offset_start), length: Int(text.offset_len))
    }

    func setMarkedText(_ string: Any, selectedRange: NSRange, replacementRange: NSRange) {
        switch string {
        case let v as NSAttributedString: markedText = NSMutableAttributedString(attributedString: v)
        case let v as String: markedText = NSMutableAttributedString(string: v)
        default: return
        }
        if keyTextAccumulator == nil { syncPreedit() }
    }

    func unmarkText() {
        if markedText.length > 0 {
            markedText.mutableString.setString("")
            syncPreedit()
        }
    }

    func validAttributesForMarkedText() -> [NSAttributedString.Key] { [] }

    func attributedSubstring(forProposedRange range: NSRange, actualRange: NSRangePointer?) -> NSAttributedString? {
        guard let surface, range.length > 0 else { return nil }
        var text = ghostty_text_s()
        guard ghostty_surface_read_selection(surface, &text) else { return nil }
        defer { ghostty_surface_free_text(surface, &text) }
        var attrs: [NSAttributedString.Key: Any] = [:]
        if let raw = ghostty_surface_quicklook_font(surface) {
            let font = Unmanaged<CTFont>.fromOpaque(raw)
            attrs[.font] = font.takeUnretainedValue()
            font.release()
        }
        return NSAttributedString(string: String(cString: text.text), attributes: attrs)
    }

    func characterIndex(for point: NSPoint) -> Int { 0 }

    func firstRect(forCharacterRange range: NSRange, actualRange: NSRangePointer?) -> NSRect {
        guard let surface else { return NSRect(origin: frame.origin, size: .zero) }
        var x: Double = 0, y: Double = 0
        var w: Double = cellSize.width, h: Double = cellSize.height
        if range.length > 0 && range != selectedRange() {
            var text = ghostty_text_s()
            if ghostty_surface_read_selection(surface, &text) {
                x = text.tl_px_x - 2
                y = text.tl_px_y + 2
                ghostty_surface_free_text(surface, &text)
            } else {
                ghostty_surface_ime_point(surface, &x, &y, &w, &h)
            }
        } else {
            ghostty_surface_ime_point(surface, &x, &y, &w, &h)
        }
        if range.length == 0, w > 0 {
            w = 0
            x += cellSize.width * Double(range.location + range.length)
        }
        let viewRect = NSRect(x: x, y: frame.height - y, width: w, height: max(h, cellSize.height))
        let winRect = convert(viewRect, to: nil)
        return window?.convertToScreen(winRect) ?? winRect
    }

    func insertText(_ string: Any, replacementRange: NSRange) {
        guard NSApp.currentEvent != nil else { return }
        var chars = ""
        switch string {
        case let v as NSAttributedString:
            chars = v.string
        case let v as NSString:
            if v.length == 1 {
                let c = v.character(at: 0)
                if UTF16.isLeadSurrogate(c) {
                    leadSurrogate = c
                    return
                } else if UTF16.isTrailSurrogate(c) {
                    if let lead = leadSurrogate { chars = String(decoding: [lead, c], as: UTF16.self) }
                    leadSurrogate = nil
                } else {
                    chars = v as String
                    leadSurrogate = nil
                }
            } else {
                chars = v as String
                leadSurrogate = nil
            }
        default:
            return
        }
        unmarkText()
        if var acc = keyTextAccumulator {
            acc.append(chars)
            keyTextAccumulator = acc
            return
        }
        if !chars.isEmpty { _ = committedText(GHOSTTY_ACTION_PRESS, chars) }
    }

    override func doCommand(by selector: Selector) {
        // Re-dispatch command-modified keys AppKit routed here so they get encoded.
        if let last = lastPerformKeyEvent, let current = NSApp.currentEvent, last == current.timestamp {
            NSApp.sendEvent(current)
        }
    }

    private func syncPreedit(clearIfNeeded: Bool = true) {
        guard let surface else { return }
        if markedText.length > 0 {
            let s = markedText.string
            s.withCString { p in ghostty_surface_preedit(surface, p, UInt(s.utf8.count)) }
        } else if clearIfNeeded {
            ghostty_surface_preedit(surface, nil, 0)
        }
    }

    // MARK: - Edit menu (used when invoked via menu / services)

    @objc func copy(_ sender: Any?) { bindingAction("copy_to_clipboard") }
    @objc func paste(_ sender: Any?) { bindingAction("paste_from_clipboard") }
    @objc override func selectAll(_ sender: Any?) { bindingAction("select_all") }

    // MARK: - Drag & drop (paste file paths / text)

    override func draggingEntered(_ sender: NSDraggingInfo) -> NSDragOperation { .copy }

    override func performDragOperation(_ sender: NSDraggingInfo) -> Bool {
        guard let surface else { return false }
        let pb = sender.draggingPasteboard
        var text: String?
        if let urls = pb.readObjects(forClasses: [NSURL.self]) as? [URL], !urls.isEmpty {
            text = urls.map { $0.isFileURL ? Clipboard.shellEscape($0.path) : $0.absoluteString }.joined(separator: " ")
        } else {
            text = pb.string(forType: .string)
        }
        guard let text, !text.isEmpty else { return false }
        text.withCString { p in ghostty_surface_paste_text(surface, p, UInt(text.utf8.count)) }
        return true
    }

    override func quickLook(with event: NSEvent) {
        guard let surface else { return super.quickLook(with: event) }
        var text = ghostty_text_s()
        guard ghostty_surface_quicklook_word(surface, &text) else { return super.quickLook(with: event) }
        defer { ghostty_surface_free_text(surface, &text) }
        guard text.text_len > 0 else { return super.quickLook(with: event) }
        var attrs: [NSAttributedString.Key: Any] = [:]
        if let raw = ghostty_surface_quicklook_font(surface) {
            let font = Unmanaged<CTFont>.fromOpaque(raw)
            attrs[.font] = font.takeUnretainedValue()
            font.release()
        }
        let pt = NSPoint(x: text.tl_px_x, y: frame.height - text.tl_px_y)
        showDefinition(for: NSAttributedString(string: String(cString: text.text), attributes: attrs), at: pt)
    }

    // MARK: - Debug helpers

    /// Visible screen contents as plain text.
    func visibleText() -> String {
        guard let surface else { return "" }
        var text = ghostty_text_s()
        let sel = ghostty_selection_s(
            top_left: ghostty_point_s(tag: GHOSTTY_POINT_VIEWPORT, coord: GHOSTTY_POINT_COORD_TOP_LEFT, x: 0, y: 0),
            bottom_right: ghostty_point_s(tag: GHOSTTY_POINT_VIEWPORT, coord: GHOSTTY_POINT_COORD_BOTTOM_RIGHT, x: 0, y: 0),
            rectangle: false)
        guard ghostty_surface_read_text(surface, sel, &text) else { return "" }
        defer { ghostty_surface_free_text(surface, &text) }
        return String(cString: text.text)
    }

    func sendText(_ s: String) {
        guard let surface else { return }
        s.withCString { p in ghostty_surface_text(surface, p, UInt(s.utf8.count)) }
    }

    var gridSize: (cols: Int, rows: Int) {
        guard let surface else { return (0, 0) }
        let s = ghostty_surface_size(surface)
        return (Int(s.columns), Int(s.rows))
    }
}

// MARK: - Input helpers

enum GhosttyInput {
    static func mods(_ flags: NSEvent.ModifierFlags) -> ghostty_input_mods_e {
        var m: UInt32 = GHOSTTY_MODS_NONE.rawValue
        if flags.contains(.shift) { m |= GHOSTTY_MODS_SHIFT.rawValue }
        if flags.contains(.control) { m |= GHOSTTY_MODS_CTRL.rawValue }
        if flags.contains(.option) { m |= GHOSTTY_MODS_ALT.rawValue }
        if flags.contains(.command) { m |= GHOSTTY_MODS_SUPER.rawValue }
        if flags.contains(.capsLock) { m |= GHOSTTY_MODS_CAPS.rawValue }
        let raw = flags.rawValue
        if raw & UInt(NX_DEVICERSHIFTKEYMASK) != 0 { m |= GHOSTTY_MODS_SHIFT_RIGHT.rawValue }
        if raw & UInt(NX_DEVICERCTLKEYMASK) != 0 { m |= GHOSTTY_MODS_CTRL_RIGHT.rawValue }
        if raw & UInt(NX_DEVICERALTKEYMASK) != 0 { m |= GHOSTTY_MODS_ALT_RIGHT.rawValue }
        if raw & UInt(NX_DEVICERCMDKEYMASK) != 0 { m |= GHOSTTY_MODS_SUPER_RIGHT.rawValue }
        return ghostty_input_mods_e(m)
    }

    static func flags(_ m: ghostty_input_mods_e) -> NSEvent.ModifierFlags {
        var f: NSEvent.ModifierFlags = []
        if m.rawValue & GHOSTTY_MODS_SHIFT.rawValue != 0 { f.insert(.shift) }
        if m.rawValue & GHOSTTY_MODS_CTRL.rawValue != 0 { f.insert(.control) }
        if m.rawValue & GHOSTTY_MODS_ALT.rawValue != 0 { f.insert(.option) }
        if m.rawValue & GHOSTTY_MODS_SUPER.rawValue != 0 { f.insert(.command) }
        return f
    }

    static func keyEvent(_ e: NSEvent, _ action: ghostty_input_action_e,
                         translationMods: NSEvent.ModifierFlags? = nil) -> ghostty_input_key_s {
        var k = ghostty_input_key_s()
        k.action = action
        k.keycode = UInt32(e.keyCode)
        k.text = nil
        k.composing = false
        k.mods = mods(e.modifierFlags)
        // Control and command never contribute to text translation.
        k.consumed_mods = mods((translationMods ?? e.modifierFlags).subtracting([.control, .command]))
        k.unshifted_codepoint = 0
        if e.type == .keyDown || e.type == .keyUp,
           let chars = e.characters(byApplyingModifiers: []), let cp = chars.unicodeScalars.first {
            k.unshifted_codepoint = cp.value
        }
        return k
    }

    /// Text for a key event, leaving control-character encoding to Ghostty.
    static func characters(_ e: NSEvent) -> String? {
        guard let chars = e.characters else { return nil }
        if chars.count == 1, let s = chars.unicodeScalars.first {
            if s.value < 0x20 { return e.characters(byApplyingModifiers: e.modifierFlags.subtracting(.control)) }
            if s.value >= 0xF700 && s.value <= 0xF8FF { return nil } // function keys (PUA)
        }
        return chars
    }

    static func button(_ n: Int) -> ghostty_input_mouse_button_e {
        switch n {
        case 0: return GHOSTTY_MOUSE_LEFT
        case 1: return GHOSTTY_MOUSE_RIGHT
        case 2: return GHOSTTY_MOUSE_MIDDLE
        case 3: return GHOSTTY_MOUSE_EIGHT
        case 4: return GHOSTTY_MOUSE_NINE
        case 5: return GHOSTTY_MOUSE_SIX
        case 6: return GHOSTTY_MOUSE_SEVEN
        case 7: return GHOSTTY_MOUSE_FOUR
        case 8: return GHOSTTY_MOUSE_FIVE
        case 9: return GHOSTTY_MOUSE_TEN
        case 10: return GHOSTTY_MOUSE_ELEVEN
        default: return GHOSTTY_MOUSE_UNKNOWN
        }
    }

    static func momentum(_ p: NSEvent.Phase) -> UInt8 {
        switch p {
        case .began: return 1
        case .stationary: return 2
        case .changed: return 3
        case .ended: return 4
        case .cancelled: return 5
        case .mayBegin: return 6
        default: return 0
        }
    }

    static func keyboardLayoutID() -> String? {
        guard let src = TISCopyCurrentKeyboardInputSource()?.takeRetainedValue(),
              let p = TISGetInputSourceProperty(src, kTISPropertyInputSourceID) else { return nil }
        return Unmanaged<CFString>.fromOpaque(p).takeUnretainedValue() as String
    }
}

// MARK: - C string helpers

func withOptionalCString<R>(_ s: String?, _ body: (UnsafePointer<CChar>?) -> R) -> R {
    guard let s, !s.isEmpty else { return body(nil) }
    return s.withCString { body($0) }
}

func withCStringPairs<R>(_ pairs: [(String, String)],
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
