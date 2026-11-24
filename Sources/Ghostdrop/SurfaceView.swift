import AppKit
import Carbon.HIToolbox

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
    private weak var runtime: GhosttyRuntime?
    private(set) var surface: GhosttySurface?
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

    init(runtime: GhosttyRuntime, profile: Profile, workingDirectory: String?, fontSize: Float = 0) {
        self.runtime = runtime
        self.profile = profile
        super.init(frame: NSRect(x: 0, y: 0, width: 800, height: 600))

        surface = runtime.createSurface(
            for: self,
            profile: profile,
            workingDirectory: workingDirectory,
            fontSize: fontSize,
            scaleFactor: Double(NSScreen.main?.backingScaleFactor ?? 2)
        )
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
        runtime?.unregisterSurface(s.id)
        s.destroy()
    }

    deinit { destroy() }

    var processExited: Bool { surface?.processExited ?? true }
    var needsConfirmQuit: Bool { surface?.needsConfirmQuit ?? false }

    func inheritedTabConfig() -> InheritedSurfaceConfig? {
        surface?.inheritedTabConfig()
    }

    func setColorScheme(dark: Bool) {
        surface?.setColorScheme(dark: dark)
    }

    func requestClose() {
        surface?.requestClose()
    }

    // MARK: - Titles & bell

    func setTitleFromTerminal(_ t: String) {
        guard surface != nil else { return }
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
        _ = surface?.bindingAction(action)
    }

    func setPasswordInput(_ mode: SecureInputMode) {
        guard surface != nil else { return }
        switch mode {
        case .on: passwordInput = true
        case .off: passwordInput = false
        case .toggle: passwordInput.toggle()
        }
        if passwordInput {
            SecureInput.shared.setScoped(ObjectIdentifier(self), focused: focused)
        } else {
            SecureInput.shared.removeScoped(ObjectIdentifier(self))
        }
    }

    func setOcclusion(visible: Bool) {
        surface?.setOcclusion(visible)
    }

    // MARK: - Focus

    func focusDidChange(_ f: Bool) {
        guard let surface, focused != f else { return }
        focused = f
        if !f { suppressNextLeftMouseUp = false }
        surface.setFocus(f)
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
        surface.setSize(width: UInt32(scaled.width), height: UInt32(scaled.height))
    }

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        viewDidChangeBackingProperties()
        updateDisplayID()
    }

    func updateDisplayID() {
        guard let surface, let screen = window?.screen,
              let id = screen.deviceDescription[NSDeviceDescriptionKey("NSScreenNumber")] as? UInt32 else { return }
        surface.setDisplayID(id)
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
        surface.setContentScale(x: fb.width / frame.width, y: fb.height / frame.height)
        let scaled = convertToBacking(contentSize ?? frame.size)
        surface.setSize(width: UInt32(scaled.width), height: UInt32(scaled.height))
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

    func setCursorShape(_ shape: MouseShape) {
        let c: NSCursor
        switch shape {
        case .defaultArrow: c = .arrow
        case .text: c = .iBeam
        case .pointer: c = .pointingHand
        case .grab: c = .openHand
        case .grabbing: c = .closedHand
        case .crosshair: c = .crosshair
        case .verticalText: c = .iBeamCursorForVerticalLayout
        case .contextMenu: c = .contextualMenu
        case .notAllowed: c = .operationNotAllowed
        case .resizeEW: c = .resizeLeftRight
        case .resizeNS: c = .resizeUpDown
        case .resizeW: c = .resizeLeft
        case .resizeE: c = .resizeRight
        case .resizeN: c = .resizeUp
        case .resizeS: c = .resizeDown
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

    private func mods(_ flags: NSEvent.ModifierFlags) -> GhosttyMods { GhosttyInput.mods(flags) }

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
        surface?.mouseButton(state: .press, button: .left, mods: mods(event.modifierFlags))
    }

    override func mouseUp(with event: NSEvent) {
        if suppressNextLeftMouseUp {
            suppressNextLeftMouseUp = false
            return
        }
        prevPressureStage = 0
        guard let surface else { return }
        surface.mouseButton(state: .release, button: .left, mods: mods(event.modifierFlags))
        surface.mousePressure(stage: 0, pressure: 0)
    }

    override func rightMouseDown(with event: NSEvent) {
        guard let surface else { return super.rightMouseDown(with: event) }
        if !surface.mouseButton(state: .press, button: .right, mods: mods(event.modifierFlags)) {
            super.rightMouseDown(with: event)
        }
    }

    override func rightMouseUp(with event: NSEvent) {
        guard let surface else { return super.rightMouseUp(with: event) }
        if !surface.mouseButton(state: .release, button: .right, mods: mods(event.modifierFlags)) {
            super.rightMouseUp(with: event)
        }
    }

    override func otherMouseDown(with event: NSEvent) {
        surface?.mouseButton(state: .press, button: GhosttyInput.button(event.buttonNumber), mods: mods(event.modifierFlags))
    }

    override func otherMouseUp(with event: NSEvent) {
        surface?.mouseButton(state: .release, button: GhosttyInput.button(event.buttonNumber), mods: mods(event.modifierFlags))
    }

    override func mouseEntered(with event: NSEvent) {
        super.mouseEntered(with: event)
        sendMousePos(event)
    }

    override func mouseExited(with event: NSEvent) {
        guard let surface else { return }
        if NSEvent.pressedMouseButtons != 0 { return }
        surface.mousePos(x: -1, y: -1, mods: mods(event.modifierFlags))
    }

    override func mouseMoved(with event: NSEvent) { sendMousePos(event) }
    override func mouseDragged(with event: NSEvent) { sendMousePos(event) }
    override func rightMouseDragged(with event: NSEvent) { sendMousePos(event) }
    override func otherMouseDragged(with event: NSEvent) { sendMousePos(event) }

    private func sendMousePos(_ event: NSEvent) {
        guard let surface else { return }
        let p = convert(event.locationInWindow, from: nil)
        surface.mousePos(x: p.x, y: frame.height - p.y, mods: mods(event.modifierFlags))
    }

    override func scrollWheel(with event: NSEvent) {
        guard let surface else { return }
        var x = event.scrollingDeltaX
        var y = event.scrollingDeltaY
        let precise = event.hasPreciseScrollingDeltas
        if precise { x *= 2; y *= 2 }
        surface.mouseScroll(x: x, y: y, precise: precise, momentum: GhosttyInput.momentum(event.momentumPhase))
    }

    /// Three-finger swipe left/right switches tabs (like iTerm2).
    override func swipe(with event: NSEvent) {
        if event.deltaX != 0 { owner?.surfaceSwiped(self, next: event.deltaX < 0) }
    }

    override func pressureChange(with event: NSEvent) {
        guard let surface else { return }
        surface.mousePressure(stage: UInt32(event.stage), pressure: Double(event.pressure))
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
        let translated = GhosttyInput.flags(surface.keyTranslationMods(mods(event.modifierFlags)))
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

        let action: KeyAction = event.isARepeat ? .repeatKey : .press
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
        _ = keyAction(.release, event: event)
    }

    override func performKeyEquivalent(with event: NSEvent) -> Bool {
        guard event.type == .keyDown, focused, let surface else { return false }

        // Is it a Ghostty keybinding? Then send it through keyDown.
        var kev = GhosttyInput.keyEvent(event, .press)
        kev.text = event.characters ?? ""
        if surface.keyIsBinding(kev) {
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
        let mod: GhosttyMods
        switch event.keyCode {
        case 0x39: mod = .caps
        case 0x38, 0x3C: mod = .shift
        case 0x3B, 0x3E: mod = .ctrl
        case 0x3A, 0x3D: mod = .alt
        case 0x37, 0x36: mod = .superMod
        default: return
        }
        if hasMarkedText() { return }
        let m = mods(event.modifierFlags)
        var action: KeyAction = .release
        if m.contains(mod) {
            let raw = event.modifierFlags.rawValue
            let sidePressed: Bool
            switch event.keyCode {
            case 0x3C: sidePressed = raw & UInt(NX_DEVICERSHIFTKEYMASK) != 0
            case 0x3E: sidePressed = raw & UInt(NX_DEVICERCTLKEYMASK) != 0
            case 0x3D: sidePressed = raw & UInt(NX_DEVICERALTKEYMASK) != 0
            case 0x36: sidePressed = raw & UInt(NX_DEVICERCMDKEYMASK) != 0
            default: sidePressed = true
            }
            if sidePressed { action = .press }
        }
        _ = keyAction(action, event: event)
    }

    private func keyAction(_ action: KeyAction, event: NSEvent, translationEvent: NSEvent? = nil,
                           text: String? = nil, composing: Bool = false) -> Bool {
        guard let surface else { return false }
        var kev = GhosttyInput.keyEvent(event, action, translationMods: translationEvent?.modifierFlags)
        kev.composing = composing
        if let text, !text.isEmpty, let first = text.unicodeScalars.first, first.value >= 0x20, first.value != 0x7F {
            kev.text = text
        }
        return surface.key(kev)
    }

    private func committedText(_ action: KeyAction, _ text: String) -> Bool {
        guard let surface else { return false }
        let kev = GhosttyKeyEvent(
            action: action,
            mods: [],
            consumedMods: [],
            keycode: 0,
            text: text,
            unshiftedCodepoint: 0,
            composing: false
        )
        return surface.key(kev)
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
        guard let sel = surface?.readSelection() else { return NSRange() }
        return NSRange(location: sel.offsetStart, length: sel.offsetLen)
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
        guard let surface, range.length > 0, let sel = surface.readSelection() else { return nil }
        var attrs: [NSAttributedString.Key: Any] = [:]
        if let font = surface.quicklookFont() {
            attrs[.font] = font
        }
        return NSAttributedString(string: sel.text, attributes: attrs)
    }

    func characterIndex(for point: NSPoint) -> Int { 0 }

    func firstRect(forCharacterRange range: NSRange, actualRange: NSRangePointer?) -> NSRect {
        guard let surface else { return NSRect(origin: frame.origin, size: .zero) }
        var x: Double = 0, y: Double = 0
        var w: Double = cellSize.width, h: Double = cellSize.height
        if range.length > 0 && range != selectedRange(), let sel = surface.readSelection() {
            x = sel.tlPxX - 2
            y = sel.tlPxY + 2
        } else {
            let pt = surface.imePoint()
            x = pt.x; y = pt.y; w = pt.width; h = pt.height
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
        if !chars.isEmpty { _ = committedText(.press, chars) }
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
            surface.preedit(markedText.string)
        } else if clearIfNeeded {
            surface.preedit(nil)
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
        surface.pasteText(text)
        return true
    }

    override func quickLook(with event: NSEvent) {
        guard let surface, let word = surface.quicklookWord() else { return super.quickLook(with: event) }
        var attrs: [NSAttributedString.Key: Any] = [:]
        if let font = surface.quicklookFont() {
            attrs[.font] = font
        }
        let pt = NSPoint(x: word.tlPxX, y: frame.height - word.tlPxY)
        showDefinition(for: NSAttributedString(string: word.text, attributes: attrs), at: pt)
    }

    // MARK: - Debug helpers

    /// Visible screen contents as plain text.
    func visibleText() -> String {
        surface?.readViewportText() ?? ""
    }

    func sendText(_ s: String) {
        surface?.text(s)
    }

    var gridSize: (cols: Int, rows: Int) {
        guard let g = surface?.gridSize else { return (0, 0) }
        return (g.cols, g.rows)
    }
}

// MARK: - Input helpers

enum GhosttyInput {
    static func mods(_ flags: NSEvent.ModifierFlags) -> GhosttyMods {
        var m: GhosttyMods = []
        if flags.contains(.shift) { m.insert(.shift) }
        if flags.contains(.control) { m.insert(.ctrl) }
        if flags.contains(.option) { m.insert(.alt) }
        if flags.contains(.command) { m.insert(.superMod) }
        if flags.contains(.capsLock) { m.insert(.caps) }
        let raw = flags.rawValue
        if raw & UInt(NX_DEVICERSHIFTKEYMASK) != 0 { m.insert(.shiftRight) }
        if raw & UInt(NX_DEVICERCTLKEYMASK) != 0 { m.insert(.ctrlRight) }
        if raw & UInt(NX_DEVICERALTKEYMASK) != 0 { m.insert(.altRight) }
        if raw & UInt(NX_DEVICERCMDKEYMASK) != 0 { m.insert(.superRight) }
        return m
    }

    static func flags(_ m: GhosttyMods) -> NSEvent.ModifierFlags {
        var f: NSEvent.ModifierFlags = []
        if m.contains(.shift) { f.insert(.shift) }
        if m.contains(.ctrl) { f.insert(.control) }
        if m.contains(.alt) { f.insert(.option) }
        if m.contains(.superMod) { f.insert(.command) }
        return f
    }

    static func keyEvent(_ e: NSEvent, _ action: KeyAction,
                         translationMods: NSEvent.ModifierFlags? = nil) -> GhosttyKeyEvent {
        var unshifted: UInt32 = 0
        if e.type == .keyDown || e.type == .keyUp,
           let chars = e.characters(byApplyingModifiers: []), let cp = chars.unicodeScalars.first {
            unshifted = cp.value
        }
        return GhosttyKeyEvent(
            action: action,
            mods: mods(e.modifierFlags),
            // Control and command never contribute to text translation.
            consumedMods: mods((translationMods ?? e.modifierFlags).subtracting([.control, .command])),
            keycode: UInt32(e.keyCode),
            text: nil,
            unshiftedCodepoint: unshifted,
            composing: false
        )
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

    static func button(_ n: Int) -> MouseButton {
        switch n {
        case 0: return .left
        case 1: return .right
        case 2: return .middle
        case 3: return .eight
        case 4: return .nine
        case 5: return .six
        case 6: return .seven
        case 7: return .four
        case 8: return .five
        case 9: return .ten
        case 10: return .eleven
        default: return .unknown
        }
    }

    static func momentum(_ p: NSEvent.Phase) -> ScrollMomentum {
        switch p {
        case .began: return .began
        case .stationary: return .stationary
        case .changed: return .changed
        case .ended: return .ended
        case .cancelled: return .cancelled
        case .mayBegin: return .mayBegin
        default: return .none
        }
    }

    static func keyboardLayoutID() -> String? {
        guard let src = TISCopyCurrentKeyboardInputSource()?.takeRetainedValue(),
              let p = TISGetInputSourceProperty(src, kTISPropertyInputSourceID) else { return nil }
        return Unmanaged<CFString>.fromOpaque(p).takeUnretainedValue() as String
    }
}
