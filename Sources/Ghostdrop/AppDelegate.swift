import AppKit
import GhosttyKit
import ServiceManagement

final class AppDelegate: NSObject, NSApplicationDelegate, GhosttyRuntimeDelegate, NSMenuDelegate {
    private(set) var settings = Settings()
    private(set) var runtime: GhosttyRuntime!
    private(set) var drop: DropController!
    private let hotkey = GlobalHotkey()
    private var statusItem: NSStatusItem?
    private var hotkeyError: String?
    private var appearanceObservation: NSKeyValueObservation?
    private let localNetwork = LocalNetworkCheck()

    func applicationDidFinishLaunching(_ notification: Notification) {
        settings = Settings.load()
        guard let rt = GhosttyRuntime(settings: settings) else {
            let a = NSAlert()
            a.messageText = "Ghostdrop failed to start"
            a.informativeText = "libghostty could not be initialized."
            a.runModal()
            NSApp.terminate(nil)
            return
        }
        runtime = rt
        rt.delegate = self
        drop = DropController(runtime: rt, settings: settings)

        hotkey.onPress = { [weak self] in self?.drop.toggle() }
        setupMainMenu()
        applySettings()
        updateColorScheme()
        appearanceObservation = NSApp.observe(\.effectiveAppearance, options: [.new]) { [weak self] _, _ in
            DispatchQueue.main.async { self?.updateColorScheme() }
        }

        DistributedNotificationCenter.default().addObserver(
            self, selector: #selector(controlMessage(_:)), name: Control.name, object: nil,
            suspensionBehavior: .deliverImmediately)

        if settings.prespawn { drop.prepare() }
        reportErrors()

        // Ask macOS for Local Network access up front (see LocalNetworkCheck).
        localNetwork.onChange = { [weak self] status in self?.localNetworkChanged(status) }
        localNetwork.run()

        if ProcessInfo.processInfo.environment["GHOSTDROP_SHOW_ON_LAUNCH"] == "1" { drop.show() }
    }

    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        // Launching the app again (Finder, Spotlight, `open -a`) toggles it.
        drop.toggle()
        return false
    }

    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        guard let app = runtime?.app, ghostty_app_needs_confirm_quit(app) else { return .terminateNow }
        if !drop.visible { drop.show() }
        Alerts.confirm(in: drop.window, title: "Quit Ghostdrop?",
                       info: "Processes are still running in some tabs. Quitting will terminate them.",
                       ok: "Quit", remember: false) { ok, _ in
            NSApp.reply(toApplicationShouldTerminate: ok)
        }
        return .terminateLater
    }

    func applicationWillTerminate(_ notification: Notification) {
        hotkey.unregister()
    }

    // MARK: - Settings

    private func applySettings() {
        hotkeyError = hotkey.register(settings.hotkey)
        if let hotkeyError { NSLog("ghostdrop: %@", hotkeyError) }
        NSApp.setActivationPolicy(settings.dockIcon ? .regular : .accessory)
        if settings.statusItem {
            if statusItem == nil {
                let item = NSStatusBar.system.statusItem(withLength: NSStatusItem.squareLength)
                item.button?.image = NSImage(systemSymbolName: "apple.terminal", accessibilityDescription: "Ghostdrop")
                    ?? NSImage(systemSymbolName: "terminal", accessibilityDescription: "Ghostdrop")
                item.button?.image?.isTemplate = true
                let menu = NSMenu()
                menu.delegate = self
                item.menu = menu
                statusItem = item
            }
        } else if let item = statusItem {
            NSStatusBar.system.removeStatusItem(item)
            statusItem = nil
        }
    }

    func reloadConfig() {
        let s = Settings.load()
        settings = s
        runtime.reload(settings: s)
        drop.apply(settings: s)
        applySettings()
        reportErrors()
    }

    private func reportErrors() {
        for e in settings.errors { NSLog("ghostdrop config: %@", e) }
        statusItem?.button?.appearsDisabled = false
    }

    private func updateColorScheme() {
        guard let app = runtime?.app else { return }
        let dark = NSApp.effectiveAppearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
        let scheme = dark ? GHOSTTY_COLOR_SCHEME_DARK : GHOSTTY_COLOR_SCHEME_LIGHT
        ghostty_app_set_color_scheme(app, scheme)
        drop?.tabs.forEach { if let s = $0.surface { ghostty_surface_set_color_scheme(s, scheme) } }
    }

    func perform(_ action: DropAction) {
        switch action {
        case .reloadConfig: reloadConfig()
        case .openConfig: openConfig()
        case .quit: NSApp.terminate(nil)
        default: drop.perform(action)
        }
    }

    @objc func openConfig() {
        let path = Settings.path
        if !FileManager.default.fileExists(atPath: path) { Settings.writeTemplate(to: path) }
        let url = URL(fileURLWithPath: path)
        let editor = NSWorkspace.shared.urlForApplication(toOpen: url)
            ?? NSWorkspace.shared.urlForApplication(withBundleIdentifier: "com.apple.TextEdit")
        if let editor {
            NSWorkspace.shared.open([url], withApplicationAt: editor, configuration: NSWorkspace.OpenConfiguration())
        } else {
            NSWorkspace.shared.open(url)
        }
    }

    private func localNetworkChanged(_ status: LocalNetworkCheck.Status) {
        guard status == .denied else { return }
        NSLog("ghostdrop: Local Network access is denied for Ghostdrop")
        statusItem?.button?.image = NSImage(systemSymbolName: "exclamationmark.triangle", accessibilityDescription: "Ghostdrop")
        // Explain once; afterwards the menu-bar item keeps a warning entry.
        let key = "localNetworkWarned"
        guard !UserDefaults.standard.bool(forKey: key) else { return }
        UserDefaults.standard.set(true, forKey: key)
        let a = NSAlert()
        a.messageText = "Ghostdrop can't access your local network"
        a.informativeText = "macOS is blocking Local Network access for Ghostdrop, so ssh/curl to devices on your LAN (or .local hosts) from its tabs will fail with \"No route to host\". Internet and VPN hosts are not affected.\n\nTurn on Ghostdrop in System Settings → Privacy & Security → Local Network."
        a.addButton(withTitle: "Open Settings")
        a.addButton(withTitle: "Not Now")
        NSApp.activate(ignoringOtherApps: true)
        if a.runModal() == .alertFirstButtonReturn { LocalNetworkCheck.openSettings() }
    }

    @objc private func openLocalNetworkSettings() { LocalNetworkCheck.openSettings() }

    // MARK: - GhosttyRuntimeDelegate

    func runtime(_ rt: GhosttyRuntime, perform action: RuntimeAction, from surface: SurfaceView?) -> Bool {
        switch action {
        case .newTab:
            drop.newTab(profile: nil)
            if !drop.visible { drop.show() }
        case .closeTab(let mode):
            drop.closeTabs(mode: mode, from: surface ?? drop.selectedView)
        case .closeWindow:
            drop.hide()
        case .gotoTab(let t):
            drop.gotoTab(t)
        case .moveTab(let n):
            drop.moveTab(by: n)
        case .toggle:
            drop.toggle()
        case .toggleFullscreen:
            drop.toggleFullscreen()
        case .resetWindowSize:
            drop.resetSize()
        case .quit:
            NSApp.terminate(nil)
        case .reloadConfig(let soft):
            if soft, let app = rt.app, let cfg = rt.config { ghostty_app_update_config(app, cfg) } else { reloadConfig() }
        case .openConfig:
            openConfig()
        case .promptTitle:
            drop.renameTab()
        case .floatWindow:
            break
        case .copyTitle:
            if let t = (surface ?? drop.selectedView)?.title {
                NSPasteboard.general.clearContents()
                NSPasteboard.general.setString(t, forType: .string)
            }
        case .startSearch(let needle):
            drop.startSearch(needle)
        case .endSearch:
            drop.closeSearch(notify: false)
        case .searchTotal(let n):
            drop.updateSearch(total: n)
        case .searchSelected(let n):
            drop.updateSearch(selected: n)
        }
        return true
    }

    func runtimeConfigDidChange(_ rt: GhosttyRuntime) {
        drop?.applyAppearance()
    }

    // MARK: - Control channel

    @objc private func controlMessage(_ n: Notification) {
        guard let cmd = n.object as? String else { return }
        let arg = (n.userInfo?["arg"] as? String).flatMap { $0.isEmpty ? nil : $0 }
        switch cmd {
        case "toggle": drop.toggle()
        case "show": drop.show()
        case "hide": drop.hide()
        case "new-tab":
            drop.newTab(profile: arg)
            drop.show()
        case "next-tab": drop.perform(.nextTab)
        case "previous-tab": drop.perform(.previousTab)
        case "reload": reloadConfig()
        default:
            guard settings.controlCommands || ProcessInfo.processInfo.environment["GHOSTDROP_DEBUG"] == "1" else { return }
            debugCommand(cmd, arg: arg)
        }
    }

    /// Commands for automated testing. Off unless `control-commands = true`.
    private func debugCommand(_ cmd: String, arg: String?) {
        switch cmd {
        case "send-text":
            drop.selectedView?.sendText((arg ?? "").replacingOccurrences(of: "\\n", with: "\n"))
        case "binding":
            if let arg { drop.selectedView?.bindingAction(arg) }
        case "key":
            // Post a real key event through the normal AppKit path, e.g. `key ctrl+shift+t`.
            for k in (arg ?? "").split(separator: " ") {
                if let combo = KeyCombo.parse(String(k)) { postKey(combo) }
            }
        case "dump":
            var state = drop.debugState()
            state["screen"] = drop.selectedView?.visibleText() ?? ""
            state["errors"] = settings.errors
            state["hotkeyError"] = hotkeyError ?? ""
            state["hotkey"] = hotkey.registered?.description ?? ""
            state["appActive"] = NSApp.isActive
            state["localNetwork"] = "\(localNetwork.status)"
            state["bundlePath"] = Bundle.main.bundlePath
            if let data = try? JSONSerialization.data(withJSONObject: state, options: [.prettyPrinted, .sortedKeys]) {
                try? data.write(to: URL(fileURLWithPath: arg ?? "/tmp/ghostdrop-dump.json"))
            }
        case "snapshot":
            Snapshot.write(window: drop.window, to: arg ?? "/tmp/ghostdrop.png")
        case "menu":
            let m = NSMenu()
            menuNeedsUpdate(m)
            try? m.items.map { $0.title }.joined(separator: "\n").write(toFile: arg ?? "/tmp/ghostdrop-menu.txt", atomically: true, encoding: .utf8)
        case "goto-tab":
            if let n = arg.flatMap(Int32.init) { drop.gotoTab(n) }
        case "resize":
            // `resize <edge> <dx> <dy>`: hit-test the edge, then drag it by (dx, dy) points.
            let parts = (arg ?? "").split(separator: " ").map(String.init)
            guard parts.count == 3, let dx = Double(parts[1]), let dy = Double(parts[2]),
                  let content = drop.window.contentView else { break }
            let b = content.bounds
            let p: NSPoint
            switch parts[0] {
            case "bottom": p = NSPoint(x: b.midX, y: 1)
            case "top": p = NSPoint(x: b.midX, y: b.maxY - 1)
            case "left": p = NSPoint(x: 1, y: b.midY)
            default: p = NSPoint(x: b.maxX - 1, y: b.midY)
            }
            let hit = content.hitTest(p)
            var log = "hit=\(hit.map { String(describing: type(of: $0)) } ?? "nil")"
            if let h = hit as? ResizeHandle {
                let start = drop.window.convertPoint(toScreen: content.convert(p, to: nil))
                drop.beginResize(h.edge, at: start)
                // Several steps, like a real drag.
                for i in 1...5 {
                    let f = CGFloat(i) / 5
                    drop.resize(to: NSPoint(x: start.x + dx * f, y: start.y + dy * f))
                }
                drop.endResize()
            }
            log += " frame=\(NSStringFromRect(drop.window.frame))"
            try? log.write(toFile: "/tmp/ghostdrop-resize.txt", atomically: true, encoding: .utf8)
        case "reset-size":
            drop.resetSize()
        case "close-tab":
            drop.perform(.closeTab)
        case "move-tab":
            if let n = arg.flatMap(Int.init) { drop.moveTab(by: n) }
        default:
            break
        }
    }

    private func postKey(_ combo: KeyCombo) {
        guard let code = combo.carbonKeyCode else { return }
        let special: [UInt16: String] = [36: "\r", 48: "\t", 53: "\u{1b}", 51: "\u{7f}", 49: " ",
                                         123: "\u{F702}", 124: "\u{F703}", 125: "\u{F701}", 126: "\u{F700}",
                                         116: "\u{F72C}", 121: "\u{F72D}"]
        var chars: String
        switch combo.key {
        case .char(let c): chars = combo.mods.contains(.shift) ? c.uppercased() : c
        case .code(let c): chars = special[c] ?? ""
        }
        let ignoring = chars
        if combo.mods.contains(.control), let s = chars.unicodeScalars.first, s.value >= 0x40, s.value < 0x80 {
            chars = String(UnicodeScalar(s.value & 0x1f)!)
        }
        let wn = drop.window.windowNumber
        for type in [NSEvent.EventType.keyDown, .keyUp] {
            if let e = NSEvent.keyEvent(with: type, location: .zero, modifierFlags: combo.mods,
                                        timestamp: ProcessInfo.processInfo.systemUptime, windowNumber: wn,
                                        context: nil, characters: chars, charactersIgnoringModifiers: ignoring,
                                        isARepeat: false, keyCode: UInt16(code)) {
                NSApp.postEvent(e, atStart: false)
            }
        }
    }

    // MARK: - Menus

    private func setupMainMenu() {
        let main = NSMenu()
        let appItem = NSMenuItem()
        let appMenu = NSMenu()
        appMenu.addItem(withTitle: "Hide Ghostdrop", action: #selector(hideDrop), keyEquivalent: "")
        appMenu.addItem(withTitle: "Quit Ghostdrop", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q")
        appItem.submenu = appMenu
        main.addItem(appItem)

        let editItem = NSMenuItem()
        let edit = NSMenu(title: "Edit")
        edit.addItem(withTitle: "Undo", action: Selector(("undo:")), keyEquivalent: "z")
        edit.addItem(withTitle: "Redo", action: Selector(("redo:")), keyEquivalent: "Z")
        edit.addItem(.separator())
        edit.addItem(withTitle: "Cut", action: #selector(NSText.cut(_:)), keyEquivalent: "x")
        edit.addItem(withTitle: "Copy", action: #selector(NSText.copy(_:)), keyEquivalent: "c")
        edit.addItem(withTitle: "Paste", action: #selector(NSText.paste(_:)), keyEquivalent: "v")
        edit.addItem(withTitle: "Select All", action: #selector(NSText.selectAll(_:)), keyEquivalent: "a")
        editItem.submenu = edit
        main.addItem(editItem)
        NSApp.mainMenu = main
    }

    func menuNeedsUpdate(_ menu: NSMenu) {
        if localNetwork.status == .denied { localNetwork.run() }
        menu.removeAllItems()
        let hk = settings.hotkey.map { " (\($0))" } ?? ""
        menu.addItem(withTitle: (drop.visible ? "Hide" : "Show") + hk, action: #selector(toggleDrop), keyEquivalent: "").target = self

        let newTab = NSMenuItem(title: "New Tab", action: nil, keyEquivalent: "")
        let sub = NSMenu()
        for name in settings.profileOrder {
            let it = sub.addItem(withTitle: name + (name == settings.defaultProfileName ? " (default)" : ""),
                                 action: #selector(newTabFromMenu(_:)), keyEquivalent: "")
            it.target = self
            it.representedObject = name
        }
        newTab.submenu = sub
        menu.addItem(newTab)
        menu.addItem(.separator())

        if let hotkeyError {
            let it = menu.addItem(withTitle: "⚠︎ " + hotkeyError, action: nil, keyEquivalent: "")
            it.isEnabled = false
        }
        if localNetwork.status == .denied {
            menu.addItem(withTitle: "⚠︎ Local Network access is off — Open Settings…",
                         action: #selector(openLocalNetworkSettings), keyEquivalent: "").target = self
        }
        if !settings.errors.isEmpty {
            menu.addItem(withTitle: "⚠︎ \(settings.errors.count) config issue(s)…", action: #selector(showErrors), keyEquivalent: "").target = self
        }
        menu.addItem(withTitle: "Open Config", action: #selector(openConfig), keyEquivalent: "").target = self
        menu.addItem(withTitle: "Reload Config", action: #selector(reloadFromMenu), keyEquivalent: "").target = self
        let login = menu.addItem(withTitle: "Launch at Login", action: #selector(toggleLoginItem), keyEquivalent: "")
        login.target = self
        login.state = SMAppService.mainApp.status == .enabled ? .on : .off
        menu.addItem(.separator())
        menu.addItem(withTitle: "Quit Ghostdrop", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "")
    }

    @objc private func toggleDrop() { drop.toggle() }
    @objc private func hideDrop() { drop.hide() }
    @objc private func reloadFromMenu() { reloadConfig() }

    @objc private func newTabFromMenu(_ sender: NSMenuItem) {
        drop.newTab(profile: sender.representedObject as? String)
        drop.show()
    }

    @objc private func showErrors() {
        let a = NSAlert()
        a.messageText = "Configuration issues"
        a.informativeText = settings.errors.joined(separator: "\n")
        NSApp.activate(ignoringOtherApps: true)
        a.runModal()
    }

    @objc private func toggleLoginItem() {
        do {
            if SMAppService.mainApp.status == .enabled {
                try SMAppService.mainApp.unregister()
            } else {
                try SMAppService.mainApp.register()
            }
        } catch {
            let a = NSAlert(error: error)
            NSApp.activate(ignoringOtherApps: true)
            a.runModal()
        }
    }
}
