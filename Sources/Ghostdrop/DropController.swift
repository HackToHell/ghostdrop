import AppKit
import GhosttyKit

/// Borderless panel that can still become key.
final class DropPanel: NSPanel {
    override var canBecomeKey: Bool { true }
    override var canBecomeMain: Bool { true }
}

/// Root content view: tab bar + terminal area + search overlay.
final class DropContentView: NSView {
    let tabBar = TabBarView()
    let terminals = NSView()
    /// Resize handles along the free edges (depends on the drop-down position).
    var handles: [ResizeHandle] = [] {
        didSet {
            oldValue.forEach { $0.removeFromSuperview() }
            handles.forEach { addSubview($0, positioned: .above, relativeTo: nil) }
            needsLayout = true
            relayout()
        }
    }
    var tabBarVisible = false { didSet { if oldValue != tabBarVisible { needsLayout = true; layoutSubtreeIfNeeded() } } }
    var tabBarAtTop = true { didSet { needsLayout = true } }

    override init(frame: NSRect) {
        super.init(frame: frame)
        terminals.autoresizesSubviews = true
        addSubview(terminals)
        addSubview(tabBar)
    }

    required init?(coder: NSCoder) { fatalError() }

    override func layout() {
        super.layout()
        relayout()
    }

    override func resizeSubviews(withOldSize oldSize: NSSize) {
        relayout()
    }

    func relayout() {
        let h = tabBarVisible ? TabBarView.height : 0
        tabBar.isHidden = !tabBarVisible
        if tabBarAtTop {
            tabBar.frame = NSRect(x: 0, y: bounds.height - h, width: bounds.width, height: h)
            terminals.frame = NSRect(x: 0, y: 0, width: bounds.width, height: bounds.height - h)
        } else {
            tabBar.frame = NSRect(x: 0, y: 0, width: bounds.width, height: h)
            terminals.frame = NSRect(x: 0, y: h, width: bounds.width, height: bounds.height - h)
        }
        let t = ResizeHandle.thickness
        for handle in handles {
            switch handle.edge {
            case .bottom: handle.frame = NSRect(x: 0, y: 0, width: bounds.width, height: t)
            case .top: handle.frame = NSRect(x: 0, y: bounds.height - t, width: bounds.width, height: t)
            case .left: handle.frame = NSRect(x: 0, y: 0, width: t, height: bounds.height)
            case .right: handle.frame = NSRect(x: bounds.width - t, y: 0, width: t, height: bounds.height)
            }
        }
    }
}

enum DropPosition: String { case top, bottom, left, right, center }

/// `GHOSTDROP_TRACE=1` logs hot-path timings to stderr.
enum Trace {
    static let enabled = ProcessInfo.processInfo.environment["GHOSTDROP_TRACE"] == "1"
    static func log(_ what: String, since t0: CFTimeInterval) {
        guard enabled else { return }
        FileHandle.standardError.write(String(format: "[trace] %@: %.2f ms\n", what, (CACurrentMediaTime() - t0) * 1000).data(using: .utf8)!)
    }
}

final class DropController: NSObject, NSWindowDelegate, SurfaceOwner, TabBarDelegate, NSSearchFieldDelegate {
    let runtime: GhosttyRuntime
    var settings: Settings
    let window: DropPanel
    private let content: DropContentView
    private(set) var tabs: [SurfaceView] = []
    private(set) var selected = 0

    /// Show/hide lifecycle. Animation completions carry the generation they
    /// were started with and are ignored once another show/hide has begun.
    private enum Phase { case hidden, showing, shown, hiding }
    private var phase = Phase.hidden
    private var generation = 0
    /// On screen or on its way there.
    var visible: Bool { phase == .showing || phase == .shown }

    private var previousApp: NSRunningApplication?
    private var keyMonitor: Any?
    private var lastScreenFrame: NSRect?
    private var fullscreen = false
    /// Size set by dragging an edge, as fractions of the screen's visible frame.
    private var manualSize: (w: CGFloat, h: CGFloat)?
    private var resizeStart: (edge: ResizeHandle.Edge, mouse: NSPoint, frame: NSRect)?
    private var handlesPosition: DropPosition?

    /// Tabs whose close needs confirming; the first one is being asked.
    private var closeConfirmations: [SurfaceView] = []

    // Search overlay. `searchView` is the surface being searched (nil = closed).
    private weak var searchView: SurfaceView?
    private let searchBar = NSView()
    private let searchField = NSSearchField()
    private let searchLabel = NSTextField(labelWithString: "")
    private var searchTimer: Timer?
    private var searchTotal = -1
    private var searchSelected = -1

    init(runtime: GhosttyRuntime, settings: Settings) {
        self.runtime = runtime
        self.settings = settings
        content = DropContentView(frame: NSRect(x: 0, y: 0, width: 800, height: 400))
        window = DropPanel(
            contentRect: NSRect(x: 0, y: 0, width: 800, height: 400),
            styleMask: [.borderless, .nonactivatingPanel],
            backing: .buffered, defer: false)
        super.init()

        window.contentView = content
        window.delegate = self
        window.isReleasedWhenClosed = false
        window.hidesOnDeactivate = false
        window.hasShadow = true
        window.isMovable = false
        window.animationBehavior = .none
        window.identifier = .init("com.ghostdrop.dropdown")
        window.setAccessibilitySubrole(.floatingWindow)
        window.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .ignoresCycle]
        window.level = .floating

        content.tabBar.delegate = self
        setupSearchBar()
        loadManualSize()
        applyAppearance()

        let nc = NotificationCenter.default
        nc.addObserver(self, selector: #selector(occlusionChanged), name: NSWindow.didChangeOcclusionStateNotification, object: window)
        nc.addObserver(self, selector: #selector(screenChanged), name: NSWindow.didChangeScreenNotification, object: window)
        nc.addObserver(self, selector: #selector(screensChanged), name: NSApplication.didChangeScreenParametersNotification, object: nil)

        // App-level key bindings take precedence over the terminal.
        keyMonitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self] e in
            guard let self, e.window === self.window else { return e }
            return self.handleKeyBinding(e) ? nil : e
        }
    }

    // MARK: - Settings / appearance

    func apply(settings: Settings) {
        self.settings = settings
        loadManualSize()
        applyAppearance()
        if visible { window.setFrame(targetFrame(on: window.screen ?? currentScreen()), display: true) }
    }

    private var configBackground: NSColor = .black

    /// Cheap: only recolors the tab bar (used on tab switches / OSC 11).
    func updateChromeColors() {
        let tb = content.tabBar
        let bg = selectedView?.backgroundColorOverride ?? configBackground
        if tb.background != bg { tb.background = bg }
    }

    func applyAppearance() {
        let bg = runtime.color("background") ?? .black
        let fg = runtime.color("foreground") ?? .white
        let opacity = CGFloat(runtime.double("background-opacity", 1))
        configBackground = bg
        let tb = content.tabBar
        tb.foreground = fg
        tb.bgAlpha = opacity
        updateChromeColors()
        content.tabBarAtTop = settings.tabBarPosition == .top

        if opacity < 1 {
            window.isOpaque = false
            window.backgroundColor = .white.withAlphaComponent(0.001)
            if window.isVisible, let app = runtime.app, runtime.uint("background-blur") != 0 {
                ghostty_set_window_background_blur(app, Unmanaged.passUnretained(window).toOpaque())
            }
        } else {
            window.isOpaque = true
            window.backgroundColor = bg
        }
        window.appearance = NSAppearance(named: isDark(bg) ? .darkAqua : .aqua)
        updateHandles(color: fg)
        updateTabBar()
    }

    private func updateHandles(color: NSColor) {
        let pos = position
        if handlesPosition != pos {
            handlesPosition = pos
            let edges: [ResizeHandle.Edge]
            switch pos {
            case .top: edges = [.bottom, .left, .right]
            case .bottom: edges = [.top, .left, .right]
            case .left: edges = [.right, .top, .bottom]
            case .right: edges = [.left, .top, .bottom]
            case .center: edges = [.top, .bottom, .left, .right]
            }
            content.handles = edges.map { e in
                let h = ResizeHandle(edge: e)
                h.controller = self
                return h
            }
        }
        content.handles.forEach { $0.color = color }
    }

    private func isDark(_ c: NSColor) -> Bool {
        let c = c.usingColorSpace(.sRGB) ?? c
        return 0.299 * c.redComponent + 0.587 * c.greenComponent + 0.114 * c.blueComponent < 0.5
    }

    // MARK: - Geometry

    var position: DropPosition {
        DropPosition(rawValue: runtime.string("quick-terminal-position") ?? "top") ?? .top
    }

    func currentScreen() -> NSScreen {
        switch runtime.string("quick-terminal-screen") ?? "mouse" {
        case "macos-menu-bar":
            return NSScreen.screens.first ?? NSScreen.main!
        case "main":
            return NSScreen.main ?? NSScreen.screens.first!
        default:
            let p = NSEvent.mouseLocation
            return NSScreen.screens.first(where: { NSMouseInRect(p, $0.frame, false) }) ?? NSScreen.main ?? NSScreen.screens.first!
        }
    }

    private func dimension(_ s: ghostty_quick_terminal_size_s, of parent: CGFloat) -> CGFloat? {
        switch s.tag {
        case GHOSTTY_QUICK_TERMINAL_SIZE_PERCENTAGE: return parent * CGFloat(s.value.percentage) / 100
        case GHOSTTY_QUICK_TERMINAL_SIZE_PIXELS: return CGFloat(s.value.pixels)
        default: return nil
        }
    }

    func configuredSize(on screen: NSScreen) -> NSSize {
        let vf = screen.visibleFrame
        let size = runtime.quickTerminalSize
        switch position {
        case .left, .right:
            return NSSize(width: dimension(size.primary, of: vf.width) ?? 400,
                          height: dimension(size.secondary, of: vf.height) ?? vf.height)
        case .top, .bottom:
            return NSSize(width: dimension(size.secondary, of: vf.width) ?? vf.width,
                          height: dimension(size.primary, of: vf.height) ?? 400)
        case .center:
            return NSSize(width: dimension(size.primary, of: vf.width) ?? 800,
                          height: dimension(size.secondary, of: vf.height) ?? 400)
        }
    }

    func targetFrame(on screen: NSScreen) -> NSRect {
        let vf = screen.visibleFrame
        if fullscreen { return vf }
        var s = manualSize.map { NSSize(width: vf.width * $0.w, height: vf.height * $0.h) }
            ?? configuredSize(on: screen)
        s.width = min(s.width, vf.width)
        s.height = min(s.height, vf.height)
        let cx = round(vf.minX + (vf.width - s.width) / 2)
        let cy = round(vf.minY + (vf.height - s.height) / 2)
        switch position {
        case .top: return NSRect(x: cx, y: vf.maxY - s.height, width: s.width, height: s.height)
        case .bottom: return NSRect(x: cx, y: vf.minY, width: s.width, height: s.height)
        case .left: return NSRect(x: vf.minX, y: cy, width: s.width, height: s.height)
        case .right: return NSRect(x: vf.maxX - s.width, y: cy, width: s.width, height: s.height)
        case .center: return NSRect(x: cx, y: cy, width: s.width, height: s.height)
        }
    }

    /// Off-screen frame the window slides from/to.
    private func hiddenFrame(for f: NSRect, on screen: NSScreen) -> NSRect {
        let sf = screen.frame
        var r = f
        switch position {
        case .top: r.origin.y = sf.maxY
        case .bottom: r.origin.y = sf.minY - f.height
        case .left: r.origin.x = sf.minX - f.width
        case .right: r.origin.x = sf.maxX
        case .center: break
        }
        return r
    }

    // MARK: - Show / hide

    func toggle() {
        if visible && window.isKeyWindow {
            hide()
        } else if visible {
            // Visible but not focused (e.g. autohide off): just focus it.
            focus()
        } else {
            show()
        }
    }

    /// Creates the first tab ahead of time so the first toggle is instant.
    func prepare() {
        let f = targetFrame(on: currentScreen())
        window.setFrame(f, display: false)
        content.relayout()
        if tabs.isEmpty { newTab(profile: nil, select: true) }
        updateOcclusion()
    }

    func show() {
        guard !visible else { return focus() }
        phase = .showing
        generation += 1
        let gen = generation
        deactivateAfterHide = false
        let t0 = CACurrentMediaTime()
        defer { Trace.log("show (main thread)", since: t0) }

        if !NSApp.isActive, let front = NSWorkspace.shared.frontmostApplication,
           front.processIdentifier != ProcessInfo.processInfo.processIdentifier {
            previousApp = front
        }
        if tabs.isEmpty { newTab(profile: nil, select: true) }

        let screen = currentScreen()
        let target = targetFrame(on: screen)
        let duration = runtime.double("quick-terminal-animation-duration", 0)
        if NSApp.isHidden { NSApp.unhide(nil) }

        // Make the surface visible to the renderer before we appear.
        updateOcclusion(forceVisible: true)

        if duration <= 0 || position == .center {
            window.setFrame(target, display: true)
            window.alphaValue = 1
            window.level = .floating
            phase = .shown
            window.makeKeyAndOrderFront(nil)
            NSApp.activate(ignoringOtherApps: true)
            focus()
            applyAppearance()
            return
        }

        window.level = .popUpMenu   // above the menu bar while sliding
        window.alphaValue = 0
        window.setFrame(hiddenFrame(for: target, on: screen), display: false)
        window.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
        focus()

        NSAnimationContext.runAnimationGroup({ ctx in
            ctx.duration = duration
            ctx.timingFunction = CAMediaTimingFunction(name: .easeOut)
            window.animator().setFrame(target, display: true)
            window.animator().alphaValue = 1
        }, completionHandler: { [weak self] in
            // A hide (or another show) started since: this one is stale.
            guard let self, self.generation == gen, self.phase == .showing else { return }
            Trace.log("show animation done", since: t0)
            self.phase = .shown
            self.window.level = .floating
            self.applyAppearance()
            self.focus()
        })
    }

    func focus() {
        guard visible else { return }
        if !window.isKeyWindow {
            window.makeKeyAndOrderFront(nil)
            NSApp.activate(ignoringOtherApps: true)
        }
        if let v = selectedView, window.firstResponder !== v, searchView == nil || window.firstResponder !== searchField.currentEditor() {
            window.makeFirstResponder(v)
        }
    }

    func hide(restoreFocus: Bool = true) {
        guard visible else { return }
        phase = .hiding
        generation += 1
        let gen = generation
        // Hiding cancels the confirmations still waiting (their tabs stay
        // open), so none of them pops the drop-down back up.
        closeConfirmations.removeAll()
        if let sheet = window.attachedSheet { window.endSheet(sheet) }

        if restoreFocus, let prev = previousApp, !prev.isTerminated {
            prev.activate(options: [])
            deactivateAfterHide = false
        } else {
            // Nothing to hand focus back to: hide the app once we're gone so the
            // next app in line becomes active.
            deactivateAfterHide = restoreFocus && NSApp.isActive
        }
        previousApp = nil

        let duration = runtime.double("quick-terminal-animation-duration", 0)
        guard let screen = window.screen, duration > 0, position != .center, window.isOnActiveSpace else {
            finishHide()
            return
        }
        window.level = .popUpMenu
        NSAnimationContext.runAnimationGroup({ ctx in
            ctx.duration = duration
            ctx.timingFunction = CAMediaTimingFunction(name: .easeIn)
            window.animator().setFrame(hiddenFrame(for: window.frame, on: screen), display: true)
            window.animator().alphaValue = 0
        }, completionHandler: { [weak self] in
            // Shown again since: don't order the window out.
            guard let self, self.generation == gen else { return }
            self.finishHide()
        })
    }

    private var deactivateAfterHide = false

    private func finishHide() {
        guard phase == .hiding else { return }
        phase = .hidden
        window.orderOut(nil)
        updateOcclusion()
        if deactivateAfterHide {
            deactivateAfterHide = false
            NSApp.hide(nil)
        }
    }

    // MARK: - Window delegate

    /// libghostty's focus state (cursor style, focus reporting, etc.) follows
    /// "window is key AND this surface is first responder".
    func syncFocus() {
        let key = window.isKeyWindow
        for v in tabs { v.focusDidChange(key && v === selectedView && window.firstResponder === v) }
    }

    func windowDidResignKey(_ notification: Notification) {
        syncFocus()
        guard phase == .shown, window.attachedSheet == nil else { return }
        guard runtime.bool("quick-terminal-autohide", true) else { return }
        // Focus went elsewhere on purpose: don't steal it back.
        hide(restoreFocus: false)
    }

    func windowDidBecomeKey(_ notification: Notification) {
        syncFocus()
        selectedView?.clearBell()
    }

    // MARK: - Mouse resizing

    static let minSize = NSSize(width: 300, height: 120)

    func beginResize(_ edge: ResizeHandle.Edge, at mouse: NSPoint) {
        fullscreen = false
        resizeStart = (edge, mouse, window.frame)
    }

    func resize(to mouse: NSPoint) {
        guard let start = resizeStart, let screen = window.screen else { return }
        let vf = screen.visibleFrame
        let dx = mouse.x - start.mouse.x, dy = mouse.y - start.mouse.y

        // Edges perpendicular to the anchored side grow symmetrically (the
        // window stays centered on that axis), so they need twice the delta.
        let pos = position
        let symmetric: Bool = {
            switch (pos, start.edge) {
            case (.center, _): return true
            case (.top, .left), (.top, .right), (.bottom, .left), (.bottom, .right): return true
            case (.left, .top), (.left, .bottom), (.right, .top), (.right, .bottom): return true
            default: return false
            }
        }()
        let k: CGFloat = symmetric ? 2 : 1
        var w = start.frame.width, h = start.frame.height
        switch start.edge {
        case .bottom: h -= dy * k
        case .top: h += dy * k
        case .left: w -= dx * k
        case .right: w += dx * k
        }
        w = min(max(w, Self.minSize.width), vf.width)
        h = min(max(h, Self.minSize.height), vf.height)
        manualSize = (w / vf.width, h / vf.height)
        window.setFrame(targetFrame(on: screen), display: true)
    }

    func endResize() {
        guard resizeStart != nil else { return }
        resizeStart = nil
        saveManualSize()
    }

    /// Identifies the config the saved size was made against; if you edit
    /// quick-terminal-size/position the saved size is dropped.
    private var sizeSignature: String {
        let s = runtime.quickTerminalSize
        func d(_ x: ghostty_quick_terminal_size_s) -> String {
            switch x.tag {
            case GHOSTTY_QUICK_TERMINAL_SIZE_PERCENTAGE: return "\(x.value.percentage)%"
            case GHOSTTY_QUICK_TERMINAL_SIZE_PIXELS: return "\(x.value.pixels)px"
            default: return "-"
            }
        }
        return "\(position.rawValue)|\(d(s.primary))|\(d(s.secondary))"
    }

    private static let sizeKey = "manualSize"

    private func saveManualSize() {
        guard let m = manualSize else {
            UserDefaults.standard.removeObject(forKey: Self.sizeKey)
            return
        }
        UserDefaults.standard.set(["w": m.w, "h": m.h, "sig": sizeSignature], forKey: Self.sizeKey)
    }

    func loadManualSize() {
        manualSize = nil
        guard let d = UserDefaults.standard.dictionary(forKey: Self.sizeKey),
              let w = d["w"] as? CGFloat, let h = d["h"] as? CGFloat, let sig = d["sig"] as? String else { return }
        if sig == sizeSignature {
            manualSize = (w, h)
        } else {
            UserDefaults.standard.removeObject(forKey: Self.sizeKey)
        }
    }

    @objc private func occlusionChanged() { updateOcclusion() }

    @objc private func screenChanged() {
        tabs.forEach { $0.updateDisplayID() }
    }

    @objc private func screensChanged() {
        guard phase == .shown else { return }
        window.setFrame(targetFrame(on: window.screen ?? currentScreen()), display: true)
    }

    private func updateOcclusion(forceVisible: Bool = false) {
        let winVisible = forceVisible || (visible && window.occlusionState.contains(.visible))
        for (i, v) in tabs.enumerated() { v.setOcclusion(visible: winVisible && i == selected) }
    }

    func toggleFullscreen() {
        fullscreen.toggle()
        guard visible else { return }
        window.setFrame(targetFrame(on: window.screen ?? currentScreen()), display: true, animate: false)
    }

    func resetSize() {
        manualSize = nil
        saveManualSize()
        fullscreen = false
        if visible { window.setFrame(targetFrame(on: window.screen ?? currentScreen()), display: true) }
    }

    // MARK: - Tabs

    var selectedView: SurfaceView? { tabs.indices.contains(selected) ? tabs[selected] : nil }

    @discardableResult
    func newTab(profile name: String?, select: Bool = true) -> SurfaceView? {
        let t0 = CACurrentMediaTime()
        defer { Trace.log("new tab", since: t0) }
        let profile = settings.profile(named: name)

        // Inherit cwd/font size from the current tab when it makes sense
        // (local shell → local shell), like Ghostty does.
        var cwd: String?
        var fontSize: Float = 0
        if let cur = selectedView, let s = cur.surface {
            let inherited = ghostty_surface_inherited_config(s, GHOSTTY_SURFACE_CONTEXT_TAB)
            fontSize = inherited.font_size
            if profile.effectiveCommand == nil, cur.profile.effectiveCommand == nil, let wd = inherited.working_directory {
                cwd = String(cString: wd)
            }
        }

        let view = SurfaceView(runtime: runtime, profile: profile, workingDirectory: cwd, fontSize: fontSize)
        guard view.surface != nil else {
            NSLog("ghostdrop: failed to create surface")
            return nil
        }
        view.owner = self
        view.frame = content.terminals.bounds
        view.autoresizingMask = [.width, .height]
        view.isHidden = true
        content.terminals.addSubview(view)
        tabs.append(view)
        if select { self.select(tabs.count - 1) } else { updateTabBar() }
        return view
    }

    func select(_ index: Int) {
        let t0 = CACurrentMediaTime()
        defer { Trace.log("select tab", since: t0) }
        guard tabs.indices.contains(index) else { return }
        let old = selectedView
        if let searched = searchView {
            searched.bindingAction("end_search")
            closeSearch(notify: false)
        }
        selected = index
        let new = tabs[index]
        new.isHidden = false
        if old !== new { old?.isHidden = true }
        updateOcclusion()
        if visible { window.makeFirstResponder(new) }
        syncFocus()
        new.clearBell()
        updateChromeColors()
        updateTabBar()
    }

    func gotoTab(_ raw: Int32) {
        guard !tabs.isEmpty else { return }
        switch raw {
        case GHOSTTY_GOTO_TAB_PREVIOUS.rawValue: select((selected - 1 + tabs.count) % tabs.count)
        case GHOSTTY_GOTO_TAB_NEXT.rawValue: select((selected + 1) % tabs.count)
        case GHOSTTY_GOTO_TAB_LAST.rawValue: select(tabs.count - 1)
        default:
            guard raw >= 1 else { return }
            select(min(Int(raw) - 1, tabs.count - 1))
        }
    }

    func moveTab(by amount: Int) {
        guard tabs.count > 1, amount != 0 else { return }
        let to = ((selected + amount) % tabs.count + tabs.count) % tabs.count
        moveTab(from: selected, to: to)
    }

    func moveTab(from: Int, to: Int) {
        guard tabs.indices.contains(from), tabs.indices.contains(to), from != to else { return }
        let v = tabs.remove(at: from)
        tabs.insert(v, at: to)
        if selected == from {
            selected = to
        } else if from < selected && to >= selected {
            selected -= 1
        } else if from > selected && to <= selected {
            selected += 1
        }
        updateTabBar()
    }

    /// Asks Ghostty to close the surface; it calls back (with confirmation needs).
    func requestClose(_ view: SurfaceView) {
        guard let s = view.surface else { return close(view) }
        ghostty_surface_request_close(s)
    }

    func close(_ view: SurfaceView) {
        guard let idx = tabs.firstIndex(where: { $0 === view }) else { return }
        if view === searchView { closeSearch(notify: false) }
        tabs.remove(at: idx)
        view.removeFromSuperview()
        view.owner = nil
        // Free on the next runloop turn: we may be inside a libghostty callback.
        DispatchQueue.main.async { view.destroy() }

        if tabs.isEmpty {
            selected = 0
            updateTabBar()
            hide()
            return
        }
        if idx < selected || selected >= tabs.count { selected = max(0, selected - 1) }
        select(min(selected, tabs.count - 1))
    }

    func closeTabs(mode: ghostty_action_close_tab_mode_e, from view: SurfaceView?) {
        guard let view, let idx = tabs.firstIndex(where: { $0 === view }) else { return }
        switch mode {
        case GHOSTTY_ACTION_CLOSE_TAB_MODE_OTHER:
            tabs.enumerated().filter { $0.offset != idx }.forEach { requestClose($0.element) }
        case GHOSTTY_ACTION_CLOSE_TAB_MODE_RIGHT:
            tabs.enumerated().filter { $0.offset > idx }.forEach { requestClose($0.element) }
        default:
            requestClose(view)
        }
    }

    func renameTab() {
        guard let v = selectedView else { return }
        let alert = NSAlert()
        alert.messageText = "Rename Tab"
        alert.informativeText = "Leave empty to use the title set by the terminal."
        let field = NSTextField(frame: NSRect(x: 0, y: 0, width: 260, height: 24))
        field.stringValue = v.titleOverride ?? ""
        alert.accessoryView = field
        alert.addButton(withTitle: "OK")
        alert.addButton(withTitle: "Cancel")
        alert.window.initialFirstResponder = field
        alert.beginSheetModal(for: window) { resp in
            guard resp == .alertFirstButtonReturn else { return }
            v.titleOverride = field.stringValue.isEmpty ? nil : field.stringValue
        }
    }

    func updateTabBar() {
        let show: Bool
        switch settings.tabBarMode {
        case .always: show = true
        case .never: show = false
        case .auto: show = tabs.count > 1
        }
        content.tabBarVisible = show
        content.tabBar.items = tabs.map { .init(title: $0.title, bell: $0.bellActive) }
        content.tabBar.selected = selected
    }

    // MARK: - SurfaceOwner

    func surfaceRequestedClose(_ view: SurfaceView, needsConfirm: Bool) {
        guard needsConfirm else {
            // Not the one on screen: its answer comes back through the queue.
            if let i = closeConfirmations.firstIndex(where: { $0 === view }), i > 0 {
                closeConfirmations.remove(at: i)
            }
            return close(view)
        }
        // One question at a time, asked with its tab in front (closing
        // several busy tabs at once would otherwise select the last one while
        // the first question is on screen).
        guard !closeConfirmations.contains(where: { $0 === view }) else { return }
        closeConfirmations.append(view)
        if closeConfirmations.count == 1 { confirmNextClose() }
    }

    private func confirmNextClose() {
        closeConfirmations.removeAll { v in !tabs.contains { $0 === v } }
        guard let view = closeConfirmations.first, let i = tabs.firstIndex(where: { $0 === view }) else { return }
        if !visible { show() }
        select(i)
        Alerts.confirm(in: window, title: "Close this tab?",
                       info: "A process is still running in this tab. Closing it will terminate the process.",
                       ok: "Close", remember: false) { [weak self] ok, _ in
            guard let self else { return }
            // Cancelled by hide(): the queue is already empty.
            guard self.closeConfirmations.first === view else { return }
            self.closeConfirmations.removeFirst()
            if ok { self.close(view) }
            self.confirmNextClose()
        }
    }

    func surfaceTitleDidChange(_ view: SurfaceView) { updateTabBar() }
    func surfaceDidRingBell(_ view: SurfaceView) { updateTabBar() }
    func surfaceBackgroundDidChange(_ view: SurfaceView) {
        if view === selectedView { updateChromeColors() }
    }

    func surfaceSwiped(_ view: SurfaceView, next: Bool) {
        perform(next ? .nextTab : .previousTab)
    }

    // MARK: - TabBarDelegate

    func tabBarSelect(_ index: Int) { select(index) }
    func tabBarClose(_ index: Int) { if tabs.indices.contains(index) { requestClose(tabs[index]) } }
    func tabBarMove(from: Int, to: Int) { moveTab(from: from, to: to) }
    func tabBarNewTab() { newTab(profile: nil) }

    // MARK: - Key bindings

    private func handleKeyBinding(_ e: NSEvent) -> Bool {
        // Don't steal keys from sheets (e.g. rename dialog).
        if window.attachedSheet != nil { return false }
        for (combo, action) in settings.bindings where combo.matches(e) {
            perform(action)
            return true
        }
        return false
    }

    func perform(_ action: DropAction) {
        switch action {
        case .newTab(let p): newTab(profile: p)
        case .closeTab: if let v = selectedView { requestClose(v) }
        case .nextTab: gotoTab(GHOSTTY_GOTO_TAB_NEXT.rawValue)
        case .previousTab: gotoTab(GHOSTTY_GOTO_TAB_PREVIOUS.rawValue)
        case .lastTab: gotoTab(GHOSTTY_GOTO_TAB_LAST.rawValue)
        case .gotoTab(let n): gotoTab(Int32(n))
        case .moveTab(let n): moveTab(by: n)
        case .toggle: toggle()
        case .hide: hide()
        case .renameTab: renameTab()
        case .ghostty(let a): selectedView?.bindingAction(a)
        case .reloadConfig, .openConfig, .quit:
            (NSApp.delegate as? AppDelegate)?.perform(action)
        case .unbind: break
        }
    }

    // MARK: - Search

    private func setupSearchBar() {
        searchBar.wantsLayer = true
        searchBar.layer?.cornerRadius = 6
        searchBar.layer?.backgroundColor = NSColor.windowBackgroundColor.withAlphaComponent(0.95).cgColor
        searchBar.isHidden = true
        searchBar.frame = NSRect(x: 0, y: 0, width: 300, height: 32)
        searchBar.autoresizingMask = [.minXMargin, .minYMargin]

        searchField.frame = NSRect(x: 6, y: 5, width: 210, height: 22)
        searchField.delegate = self
        searchField.sendsSearchStringImmediately = true
        searchField.target = self
        searchField.action = #selector(searchChanged)
        searchField.placeholderString = "Search"
        searchBar.addSubview(searchField)

        searchLabel.frame = NSRect(x: 220, y: 8, width: 74, height: 16)
        searchLabel.alignment = .center
        searchLabel.font = .monospacedDigitSystemFont(ofSize: 11, weight: .regular)
        searchLabel.textColor = .secondaryLabelColor
        searchBar.addSubview(searchLabel)
        content.terminals.addSubview(searchBar)
    }

    func startSearch(_ needle: String, in view: SurfaceView?) {
        guard let view = view ?? selectedView, tabs.contains(where: { $0 === view }) else { return }
        if searchView !== view {
            searchView?.bindingAction("end_search")
            searchTotal = -1
            searchSelected = -1
            updateSearch()
        }
        searchView = view
        let b = content.terminals.bounds
        searchBar.frame.origin = NSPoint(x: b.maxX - searchBar.frame.width - 10, y: b.maxY - searchBar.frame.height - 10)
        searchBar.isHidden = false
        content.terminals.addSubview(searchBar, positioned: .above, relativeTo: nil)
        if !needle.isEmpty { searchField.stringValue = needle }
        window.makeFirstResponder(searchField)
        searchChanged()
    }

    @objc private func searchChanged() {
        let needle = searchField.stringValue
        searchTimer?.invalidate()
        let send = { [weak self] in self?.searchView?.bindingAction("search:\(needle)") }
        if needle.isEmpty || needle.count >= 3 {
            send()
        } else {
            searchTimer = Timer.scheduledTimer(withTimeInterval: 0.3, repeats: false) { _ in send() }
        }
    }

    func closeSearch(notify: Bool = true) {
        searchTimer?.invalidate()
        guard let searched = searchView else { return }
        searchView = nil
        searchBar.isHidden = true
        searchTotal = -1
        searchSelected = -1
        searchLabel.stringValue = ""
        if notify { searched.bindingAction("end_search") }
        if let v = selectedView { window.makeFirstResponder(v) }
    }

    /// libghostty ended the search in `view` (nil: whichever is open).
    func searchEnded(in view: SurfaceView?) {
        guard view == nil || view === searchView else { return }
        closeSearch(notify: false)
    }

    /// Search results from `view`; results from any other surface are stale.
    func updateSearch(total: Int? = nil, selected sel: Int? = nil, from view: SurfaceView? = nil) {
        if let view, view !== searchView { return }
        if let total { searchTotal = total }
        if let sel { searchSelected = sel }
        if searchTotal < 0 {
            searchLabel.stringValue = ""
        } else if searchSelected >= 0 {
            searchLabel.stringValue = "\(searchSelected + 1)/\(searchTotal)"
        } else {
            searchLabel.stringValue = "-/\(searchTotal)"
        }
    }

    func control(_ control: NSControl, textView: NSTextView, doCommandBy sel: Selector) -> Bool {
        switch sel {
        case #selector(NSResponder.insertNewline(_:)):
            let back = NSApp.currentEvent?.modifierFlags.contains(.shift) ?? false
            searchView?.bindingAction(back ? "navigate_search:previous" : "navigate_search:next")
            return true
        case #selector(NSResponder.cancelOperation(_:)):
            closeSearch()
            return true
        default:
            return false
        }
    }

    // MARK: - Debug

    func debugState() -> [String: Any] {
        [
            "visible": visible,
            "key": window.isKeyWindow,
            "frame": NSStringFromRect(window.frame),
            "selected": selected,
            "sheet": window.attachedSheet != nil,
            "search": searchView != nil ? "\(searchField.stringValue) [\(searchLabel.stringValue)]" : "",
            "phase": "\(phase)",
            "closeConfirmations": closeConfirmations.count,
            "tabBar": content.tabBarVisible,
            "manualSize": manualSize.map { String(format: "%.3f x %.3f", $0.w, $0.h) } ?? "",
            "handles": content.handles.map { "\($0.edge)" },
            "tabs": tabs.map { v -> [String: Any] in
                let g = v.gridSize
                return ["title": v.title, "profile": v.profile.name, "cols": g.cols, "rows": g.rows,
                        "focused": v.focused, "exited": v.processExited, "pwd": v.pwd ?? ""]
            },
        ]
    }
}
