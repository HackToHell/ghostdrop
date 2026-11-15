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
    private(set) var visible = false
    private var animating = false
    private var previousApp: NSRunningApplication?
    private var keyMonitor: Any?
    private var lastScreenFrame: NSRect?
    private var fullscreen = false
    private var userResized: NSSize?

    // Search overlay.
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
            styleMask: [.borderless, .nonactivatingPanel, .resizable],
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
        userResized = nil
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
        updateTabBar()
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
        var s = userResized ?? configuredSize(on: screen)
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
        visible = true
        let t0 = CACurrentMediaTime()
        defer { Trace.log("show (main thread)", since: t0) }

        if !NSApp.isActive, let front = NSWorkspace.shared.frontmostApplication,
           front.processIdentifier != ProcessInfo.processInfo.processIdentifier {
            previousApp = front
        }
        if tabs.isEmpty { newTab(profile: nil, select: true) }

        let screen = currentScreen()
        let target = targetFrame(on: screen)
        let duration = runtime.double("quick-terminal-animation-duration", 0.1)
        if NSApp.isHidden { NSApp.unhide(nil) }

        // Make the surface visible to the renderer before we appear.
        updateOcclusion(forceVisible: true)

        if duration <= 0 || position == .center {
            window.setFrame(target, display: true)
            window.alphaValue = 1
            window.level = .floating
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

        animating = true
        NSAnimationContext.runAnimationGroup({ ctx in
            ctx.duration = duration
            ctx.timingFunction = CAMediaTimingFunction(name: .easeOut)
            window.animator().setFrame(target, display: true)
            window.animator().alphaValue = 1
        }, completionHandler: { [weak self] in
            guard let self else { return }
            Trace.log("show animation done", since: t0)
            self.animating = false
            guard self.visible else { return }
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
        if let v = selectedView, window.firstResponder !== v, searchBar.isHidden || window.firstResponder !== searchField.currentEditor() {
            window.makeFirstResponder(v)
        }
    }

    func hide(restoreFocus: Bool = true) {
        guard visible else { return }
        visible = false
        if window.attachedSheet != nil { window.endSheet(window.attachedSheet!) }

        if restoreFocus, let prev = previousApp, !prev.isTerminated {
            prev.activate(options: [])
            deactivateAfterHide = false
        } else {
            // Nothing to hand focus back to: hide the app once we're gone so the
            // next app in line becomes active.
            deactivateAfterHide = restoreFocus && NSApp.isActive
        }
        previousApp = nil

        let duration = runtime.double("quick-terminal-animation-duration", 0.1)
        guard let screen = window.screen, duration > 0, position != .center, window.isOnActiveSpace else {
            finishHide()
            return
        }
        window.level = .popUpMenu
        animating = true
        NSAnimationContext.runAnimationGroup({ ctx in
            ctx.duration = duration
            ctx.timingFunction = CAMediaTimingFunction(name: .easeIn)
            window.animator().setFrame(hiddenFrame(for: window.frame, on: screen), display: true)
            window.animator().alphaValue = 0
        }, completionHandler: { [weak self] in
            self?.animating = false
            self?.finishHide()
        })
    }

    private var deactivateAfterHide = false

    private func finishHide() {
        guard !visible else { return }
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
        guard visible, !animating, window.attachedSheet == nil else { return }
        guard runtime.bool("quick-terminal-autohide", true) else { return }
        // Focus went elsewhere on purpose: don't steal it back.
        hide(restoreFocus: false)
    }

    func windowDidBecomeKey(_ notification: Notification) {
        syncFocus()
        selectedView?.clearBell()
    }

    func windowDidResize(_ notification: Notification) {
        guard visible, !animating, !fullscreen, let screen = window.screen else { return }
        let f = window.frame
        if f.size != targetFrame(on: screen).size { userResized = f.size }
        // Keep it anchored to its edge.
        let t = targetFrame(on: screen)
        if t.origin != f.origin { window.setFrameOrigin(t.origin) }
    }

    @objc private func occlusionChanged() { updateOcclusion() }

    @objc private func screenChanged() {
        tabs.forEach { $0.updateDisplayID() }
    }

    @objc private func screensChanged() {
        guard visible, !animating else { return }
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
        userResized = nil
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
        if !searchBar.isHidden {
            old?.bindingAction("end_search")
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
        guard needsConfirm else { return close(view) }
        if !visible { show() }
        if let i = tabs.firstIndex(where: { $0 === view }) { select(i) }
        Alerts.confirm(in: window, title: "Close this tab?",
                       info: "A process is still running in this tab. Closing it will terminate the process.",
                       ok: "Close", remember: false) { [weak self] ok, _ in
            if ok { self?.close(view) }
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

    func startSearch(_ needle: String) {
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
        let send = { [weak self] in self?.selectedView?.bindingAction("search:\(needle)") }
        if needle.isEmpty || needle.count >= 3 {
            send()
        } else {
            searchTimer = Timer.scheduledTimer(withTimeInterval: 0.3, repeats: false) { _ in send() }
        }
    }

    func closeSearch(notify: Bool = true) {
        searchTimer?.invalidate()
        guard !searchBar.isHidden else { return }
        searchBar.isHidden = true
        searchTotal = -1
        searchSelected = -1
        searchLabel.stringValue = ""
        if notify { selectedView?.bindingAction("end_search") }
        if let v = selectedView { window.makeFirstResponder(v) }
    }

    func updateSearch(total: Int? = nil, selected sel: Int? = nil) {
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
            selectedView?.bindingAction(back ? "navigate_search:previous" : "navigate_search:next")
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
            "search": !searchBar.isHidden ? "\(searchField.stringValue) [\(searchLabel.stringValue)]" : "",
            "tabBar": content.tabBarVisible,
            "tabs": tabs.map { v -> [String: Any] in
                let g = v.gridSize
                return ["title": v.title, "profile": v.profile.name, "cols": g.cols, "rows": g.rows,
                        "focused": v.focused, "exited": v.processExited, "pwd": v.pwd ?? ""]
            },
        ]
    }
}
