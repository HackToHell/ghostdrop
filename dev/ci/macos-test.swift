import AppKit

/// Automated macOS integration & regression test suite for Ghostdrop.
/// Exercises the FFI isolation boundary, re-entrancy guards, async clipboard,
/// unified prompt queue, non-blocking alerts, and live libghostty callbacks.
final class MacOSTestRunner: NSObject, NSApplicationDelegate, GhosttyRuntimeDelegate {
    private var settings: Settings!
    private var runtime: GhosttyRuntime!
    private var drop: DropController!
    private var reloadCount = 0
    private var softReloadCount = 0
    private var savedPasteboard: String?

    private func fail(_ msg: String, file: StaticString = #file, line: UInt = #line) -> Never {
        restorePasteboard()
        fputs("FAIL (\(file):\(line)): \(msg)\n", stderr)
        exit(1)
    }

    private func check(_ cond: @autoclosure () -> Bool, _ msg: String, file: StaticString = #file, line: UInt = #line) {
        if !cond() { fail(msg, file: file, line: line) }
    }

    private func pass(_ name: String) {
        fputs("PASS: \(name)\n", stderr)
    }

    private func restorePasteboard() {
        let pb = NSPasteboard.general
        pb.clearContents()
        if let savedPasteboard { pb.setString(savedPasteboard, forType: .string) }
    }

    private func pump(_ seconds: Double) {
        let end = Date().addingTimeInterval(seconds)
        while Date() < end {
            RunLoop.current.run(mode: .default, before: Date().addingTimeInterval(0.01))
        }
    }

    private func waitFor(_ timeout: Double, _ cond: () -> Bool) -> Bool {
        let end = Date().addingTimeInterval(timeout)
        while Date() < end {
            if cond() { return true }
            RunLoop.current.run(mode: .default, before: Date().addingTimeInterval(0.01))
        }
        return cond()
    }

    private func findButtons(in view: NSView) -> [NSButton] {
        var out: [NSButton] = []
        if let b = view as? NSButton { out.append(b) }
        for sub in view.subviews { out.append(contentsOf: findButtons(in: sub)) }
        return out
    }

    private func findEditableTextField(in view: NSView) -> NSTextField? {
        if let tf = view as? NSTextField, tf.isEditable { return tf }
        for sub in view.subviews {
            if let found = findEditableTextField(in: sub) { return found }
        }
        return nil
    }

    private func clickSheetButton(_ title: String, file: StaticString = #file, line: UInt = #line) {
        guard let sheet = drop.window.attachedSheet, let content = sheet.contentView else {
            fail("expected attached sheet to click '\(title)'", file: file, line: line)
        }
        let buttons = findButtons(in: content)
        guard let btn = buttons.first(where: { $0.title == title }) else {
            fail("button '\(title)' not found in sheet (found: \(buttons.map(\.title)))", file: file, line: line)
        }
        btn.performClick(nil)
    }

    // MARK: - GhosttyRuntimeDelegate (mirrors AppDelegate)

    func runtime(_ rt: GhosttyRuntime, perform action: RuntimeAction, from surface: SurfaceView?) -> Bool {
        check(!GhosttyFFI.isInCall, "delegate perform invoked while GhosttyFFI.isInCall")
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
            break
        case .reloadConfig(let soft):
            if soft {
                rt.softReload()
                softReloadCount += 1
            } else {
                let s = Settings.load()
                settings = s
                rt.reload(settings: s)
                drop.apply(settings: s)
                reloadCount += 1
            }
        case .openConfig, .floatWindow:
            break
        case .promptTitle:
            drop.renameTab(for: surface ?? drop.selectedView)
        case .copyTitle:
            if let t = (surface ?? drop.selectedView)?.title {
                NSPasteboard.general.clearContents()
                NSPasteboard.general.setString(t, forType: .string)
            }
        case .startSearch(let needle):
            drop.startSearch(needle, in: surface)
        case .endSearch:
            drop.searchEnded(in: surface)
        case .searchTotal(let n):
            drop.updateSearch(total: n, from: surface)
        case .searchSelected(let n):
            drop.updateSearch(selected: n, from: surface)
        }
        return true
    }

    func runtimeConfigDidChange(_ rt: GhosttyRuntime) {
        check(!GhosttyFFI.isInCall, "runtimeConfigDidChange invoked while GhosttyFFI.isInCall")
        drop?.applyAppearance()
    }

    func runtime(
        _ rt: GhosttyRuntime,
        confirm title: String,
        info: String,
        preview: String?,
        ok: String,
        remember: Bool,
        from surface: SurfaceView?,
        completion: @escaping (_ ok: Bool, _ remember: Bool) -> Void
    ) {
        check(!GhosttyFFI.isInCall, "delegate confirm invoked while GhosttyFFI.isInCall")
        drop.confirm(title: title, info: info, preview: preview, ok: ok, remember: remember, from: surface, completion: completion)
    }

    // MARK: - Test suite

    func applicationDidFinishLaunching(_ notification: Notification) {
        // Hard deadlock watchdog: if anything hangs, fail fast with non-zero exit.
        DispatchQueue.global().asyncAfter(deadline: .now() + 30) {
            fputs("FAIL: DEADLOCK TIMEOUT (30s)\n", stderr)
            _exit(2)
        }

        savedPasteboard = NSPasteboard.general.string(forType: .string)

        let tmpDir = FileManager.default.temporaryDirectory
            .appendingPathComponent("ghostdrop-macos-test-\(ProcessInfo.processInfo.processIdentifier)")
        try? FileManager.default.createDirectory(at: tmpDir, withIntermediateDirectories: true)
        let cfgPath = tmpDir.appendingPathComponent("config").path
        try! """
        hotkey = none
        status-item = false
        inherit-ghostty-config = false
        prespawn = false
        quick-terminal-autohide = false
        quick-terminal-animation-duration = 0
        clipboard-read = ask
        clipboard-write = ask
        clipboard-paste-protection = true
        copy-on-select = clipboard
        """.write(toFile: cfgPath, atomically: true, encoding: .utf8)
        setenv("GHOSTDROP_CONFIG", cfgPath, 1)

        settings = Settings.load()
        guard let rt = GhosttyRuntime(settings: settings) else { fail("GhosttyRuntime init failed") }
        runtime = rt
        rt.delegate = self
        drop = DropController(runtime: rt, settings: settings)

        // Show window cleanly.
        drop.show(animated: false)
        pump(0.1)

        testUntrustedLinkClick()
        testCopyOnSelect()
        testUnsafePasteAndEmptyClipboard()
        testOSC52Confirmations()
        testHiddenWindowPromptAndHideCancel()
        testQueuedBusyCloseAndTabExitCleanup()
        testMixedPromptQueueRenameAndQuit()
        testSearchLifecycle()
        testReloadConfig()
        testNonBlockingStandaloneAlert()
        testDestroyWithPendingClipboardRequest()

        // Clean up surfaces and temp config.
        for v in Array(drop.tabs) { v.destroy() }
        restorePasteboard()
        try? FileManager.default.removeItem(at: tmpDir)
        fputs("ALL MACOS TESTS PASSED\n", stderr)
        exit(0)
    }

    private func addCommandTab(name: String, command: String, waitText: String) -> SurfaceView {
        var prof = Profile(name: name)
        prof.command = command
        settings.profiles[name] = prof
        guard let view = drop.newTab(profile: name, select: true) else {
            fail("newTab failed for \(name)")
        }
        drop.window.makeFirstResponder(view)
        view.setOcclusion(visible: true)
        drop.syncFocus()
        check(waitFor(5.0, { view.visibleText().contains(waitText) }),
              "tab \(name) never rendered '\(waitText)'; screen='\(view.visibleText())'")
        return view
    }

    /// 1. Exact deadlock repro: Cmd-click an OSC 8 file:// hyperlink while focused.
    ///    libghostty holds renderer_state.mutex during mouseButtonCallback -> open_url.
    private func testUntrustedLinkClick() {
        let cmd = "/bin/sh -c 'printf \"\\033[H\\033]8;;file:///tmp/ghostdrop-link-test\\033\\\\CLICKME\\033]8;;\\033\\\\\\n\"; sleep 30'"
        let view = addCommandTab(name: "link", command: cmd, waitText: "CLICKME")
        guard let surface = view.surface else { fail("missing surface") }

        let wasKey = drop.window.isKeyWindow
        surface.mousePos(x: 15, y: 10, mods: .superMod)
        check(waitFor(2.0, { view.hoverURL == "file:///tmp/ghostdrop-link-test" }),
              "expected hoverURL file:///tmp/ghostdrop-link-test, got \(view.hoverURL ?? "nil")")

        // Must return immediately without deadlocking on renderer_state.mutex.
        surface.mouseButton(state: .press, button: .left, mods: .superMod)
        surface.mouseButton(state: .release, button: .left, mods: .superMod)
        check(!GhosttyFFI.isInCall, "isInCall must be false after mouseButton returns")

        check(waitFor(2.0, { self.drop.window.attachedSheet != nil }),
              "expected confirmation sheet for untrusted file:// link")
        check(NSApp.modalWindow == nil, "sheet must not spin modal runloop")
        if wasKey {
            check(!view.focused, "surface should have lost focus when sheet became key")
        }

        clickSheetButton("Cancel")
        check(waitFor(2.0, { self.drop.window.attachedSheet == nil }), "link sheet did not dismiss")
        check((drop.debugState()["prompts"] as? Int) == 0, "expected 0 prompts after cancel")
        pass("1. untrusted OSC 8 link click (no mutex deadlock)")
    }

    /// 2. Copy-on-select during left mouse drag/release (also runs under renderer_state.mutex).
    private func testCopyOnSelect() {
        guard let view = drop.selectedView, let surface = view.surface else { fail("missing tab") }
        let pb = NSPasteboard.general
        pb.clearContents()
        pb.setString("SENTINEL", forType: .string)

        surface.mousePos(x: 2, y: 10, mods: [])
        surface.mouseButton(state: .press, button: .left, mods: [])
        surface.mousePos(x: 120, y: 10, mods: [])
        surface.mouseButton(state: .release, button: .left, mods: [])

        check(waitFor(2.0, { pb.string(forType: .string)?.contains("CLICKME") == true }),
              "expected copy-on-select to write CLICKME to pasteboard, got '\(pb.string(forType: .string) ?? "nil")'")
        pass("2. copy-on-select under renderer_state.mutex")
    }

    /// 3 & 4. Async clipboard read: unsafe paste confirmation (cancel + approve) and empty clipboard.
    private func testUnsafePasteAndEmptyClipboard() {
        let cmd = "/bin/sh -c 'echo READY; while IFS= read -r line; do echo \"GOT:$line\"; done'"
        let view = addCommandTab(name: "paste", command: cmd, waitText: "READY")

        // Empty clipboard: must complete cleanly with no prompt.
        let pb = NSPasteboard.general
        pb.clearContents()
        view.bindingAction("paste_from_clipboard")
        pump(0.15)
        check(drop.window.attachedSheet == nil, "empty clipboard must not prompt")

        // Multiline paste: must trigger unsafe paste confirmation sheet.
        pb.clearContents()
        pb.setString("hello1\nhello2\n", forType: .string)
        view.bindingAction("paste_from_clipboard")
        check(waitFor(2.0, { self.drop.window.attachedSheet != nil }), "expected unsafe paste sheet")
        clickSheetButton("Cancel")
        check(waitFor(2.0, { self.drop.window.attachedSheet == nil }), "unsafe paste sheet did not dismiss on Cancel")
        pump(0.1)
        check(!view.visibleText().contains("GOT:hello1"), "cancelled paste should not reach shell")

        // Approve paste: must deliver text to shell.
        view.bindingAction("paste_from_clipboard")
        check(waitFor(2.0, { self.drop.window.attachedSheet != nil }), "expected second unsafe paste sheet")
        clickSheetButton("Paste")
        check(waitFor(2.0, { self.drop.window.attachedSheet == nil }), "unsafe paste sheet did not dismiss on Paste")
        check(waitFor(3.0, { view.visibleText().contains("GOT:hello1") && view.visibleText().contains("GOT:hello2") }),
              "approved paste did not reach shell: '\(view.visibleText())'")
        pass("3. async clipboard read, empty clipboard, and unsafe paste confirmation")
    }

    /// 5. OSC 52 write and read confirmations under `ask` policy.
    private func testOSC52Confirmations() {
        let pb = NSPasteboard.general
        pb.clearContents()
        pb.setString("BEFORE", forType: .string)

        // Emit OSC 52 write ("hello" = aGVsbG8=), then OSC 52 read ("?").
        let cmd = "/bin/sh -c 'echo OSC52; printf \"\\033]52;c;aGVsbG8=\\033\\\\\"; sleep 0.2; printf \"\\033]52;c;?\\033\\\\\"; sleep 30'"
        _ = addCommandTab(name: "osc52", command: cmd, waitText: "OSC52")

        // First prompt: clipboard write.
        check(waitFor(3.0, { self.drop.window.attachedSheet != nil }), "expected OSC 52 write confirmation sheet")
        clickSheetButton("Allow")
        check(waitFor(2.0, { pb.string(forType: .string) == "hello" }),
              "expected pasteboard == 'hello', got '\(pb.string(forType: .string) ?? "nil")'")

        // Second prompt: clipboard read (deny).
        check(waitFor(3.0, { self.drop.window.attachedSheet != nil }), "expected OSC 52 read confirmation sheet")
        clickSheetButton("Cancel")
        check(waitFor(2.0, { self.drop.window.attachedSheet == nil }), "OSC 52 read sheet did not close")
        pass("4. OSC 52 write & read confirmation sheets")
    }

    /// 6. Hidden window confirmation pops open sheet (never runModal); hide() cancels queued prompts.
    private func testHiddenWindowPromptAndHideCancel() {
        drop.hide(restoreFocus: false)
        check(!drop.visible, "drop should be hidden")

        var firstCancelled = false
        var secondCancelled = false
        var firstCalls = 0
        var secondCalls = 0

        drop.confirm(title: "Prompt 1", info: "First hidden prompt", ok: "OK", remember: false, from: nil) { ok, _ in
            firstCalls += 1
            firstCancelled = !ok
        }
        drop.confirm(title: "Prompt 2", info: "Queued prompt", ok: "OK", remember: false, from: nil) { ok, _ in
            secondCalls += 1
            secondCancelled = !ok
        }

        check(drop.visible, "prompt while hidden must show drop-down")
        check(waitFor(2.0, { self.drop.window.attachedSheet != nil }), "hidden prompt must attach sheet")
        check(NSApp.modalWindow == nil, "hidden prompt must not use runModal()")
        check((drop.debugState()["prompts"] as? Int) == 2, "expected 2 prompts in queue")

        drop.hide(restoreFocus: false)
        pump(0.15)
        check(!drop.visible, "drop should be hidden again")
        check(firstCalls == 1 && firstCancelled, "active prompt must resolve cancelled exactly once (calls=\(firstCalls))")
        check(secondCalls == 1 && secondCancelled, "queued prompt must resolve cancelled exactly once (calls=\(secondCalls))")
        check((drop.debugState()["prompts"] as? Int) == 0, "prompt queue must be empty after hide()")

        drop.show(animated: false)
        pump(0.05)
        pass("5. hidden-window sheet (no runModal) and hide() cancels queued prompts once")
    }

    /// 7. Busy close confirmations queue one at a time, deduplicate, and clean up if a tab exits mid-prompt.
    private func testQueuedBusyCloseAndTabExitCleanup() {
        // Close existing tabs cleanly first.
        for v in Array(drop.tabs) { drop.close(v) }
        drop.show(animated: false)

        let tabA = addCommandTab(name: "busyA", command: "/bin/sh -c 'echo BUSYA; sleep 30'", waitText: "BUSYA")
        let tabB = addCommandTab(name: "busyB", command: "/bin/sh -c 'echo BUSYB; sleep 30'", waitText: "BUSYB")

        drop.requestClose(tabA)
        drop.requestClose(tabA) // duplicate -> deduplicated
        drop.requestClose(tabB)

        check(waitFor(2.0, { self.drop.window.attachedSheet != nil && (self.drop.debugState()["prompts"] as? Int) == 2 }),
              "expected 2 queued close prompts, got \(drop.debugState()["prompts"] ?? "nil")")
        check(drop.selectedView === tabA, "tabA should be selected for its close sheet")

        // Simulate tabA exiting while its close sheet is open -> dismisses sheet 1 and advances to tabB!
        drop.close(tabA)
        check(waitFor(2.0, { self.drop.selectedView === tabB && self.drop.window.attachedSheet != nil }),
              "closing tabA mid-sheet should advance to tabB's close sheet")
        check((drop.debugState()["prompts"] as? Int) == 1, "expected 1 remaining prompt for tabB")

        clickSheetButton("Cancel")
        check(waitFor(2.0, { self.drop.window.attachedSheet == nil }), "tabB close sheet did not dismiss")
        check(drop.tabs.contains(where: { $0 === tabB }), "tabB should remain open after Cancel")
        pass("6. queued busy close confirmations, deduplication, and active-tab exit cleanup")
    }

    /// 8. Mixed prompts (renameTab + confirmQuit) queue cleanly without colliding.
    private func testMixedPromptQueueRenameAndQuit() {
        guard let tab = drop.selectedView else { fail("missing selected tab") }
        var quitReply: Bool?
        var quitCalls = 0

        drop.renameTab(for: tab)
        drop.confirmQuit { ok in
            quitCalls += 1
            quitReply = ok
        }
        check(waitFor(2.0, { self.drop.window.attachedSheet != nil }), "expected rename sheet")
        check((drop.debugState()["prompts"] as? Int) == 2, "expected 2 mixed prompts queued")

        if let sheet = drop.window.attachedSheet, let content = sheet.contentView,
           let tf = findEditableTextField(in: content) {
            tf.stringValue = "renamed-tab"
        } else {
            fail("rename text field not found in sheet")
        }
        clickSheetButton("OK")

        // Wait for rename to apply and quit sheet to open next.
        check(waitFor(2.0, { tab.title == "renamed-tab" }), "tab title was not updated to 'renamed-tab'")
        check(waitFor(2.0, { self.drop.window.attachedSheet != nil && (self.drop.debugState()["prompts"] as? Int) == 1 }),
              "expected quit sheet after rename sheet")

        clickSheetButton("Cancel")
        check(waitFor(2.0, { self.drop.window.attachedSheet == nil && quitCalls == 1 }),
              "quit reply did not fire cleanly (calls=\(quitCalls))")
        check(quitReply == false, "expected quitReply == false on Cancel")
        pass("7. mixed prompt queue (rename + quit) with exact-once resolution")
    }

    /// 9. Search lifecycle and closing searched tab.
    private func testSearchLifecycle() {
        let view = addCommandTab(name: "search", command: "/bin/sh -c 'echo NEEDLE_XYZ; sleep 30'", waitText: "NEEDLE_XYZ")
        view.bindingAction("start_search")
        check(waitFor(2.0, { (self.drop.debugState()["search"] as? String)?.isEmpty == false }), "search bar did not open")
        drop.close(view)
        pump(0.05)
        check((drop.debugState()["search"] as? String) == "", "closing searched tab should close search bar")
        pass("8. search lifecycle & tab-close cleanup")
    }

    /// 10. Config reload (hard and soft) via libghostty binding action.
    private func testReloadConfig() {
        guard let view = drop.selectedView else { fail("missing tab") }
        let before = reloadCount
        view.bindingAction("reload_config")
        check(waitFor(2.0, { self.reloadCount > before }), "reload_config action did not run")
        runtime.softReload()
        pass("9. reload_config (hard & soft)")
    }

    /// 11. Standalone non-blocking alert (no runModal nested runloop).
    private func testNonBlockingStandaloneAlert() {
        let a = NSAlert()
        a.messageText = "Non-blocking test"
        a.informativeText = "Should not enter modal runloop"
        a.addButton(withTitle: "Primary")
        a.addButton(withTitle: "Secondary")
        var got: NSApplication.ModalResponse?
        Alerts.present(a, activate: false) { resp in got = resp }
        check(NSApp.modalWindow == nil, "Alerts.present must not spin runModal()")
        check(Alerts.activeAlertCount == 1, "expected 1 active non-blocking alert")
        a.buttons[1].performClick(nil)
        check(got == .alertSecondButtonReturn, "expected alertSecondButtonReturn, got \(String(describing: got))")
        check(Alerts.activeAlertCount == 0, "non-blocking alert host leaked")
        pass("10. non-blocking standalone NSAlert")
    }

    /// 12. Surface destroyed while an async clipboard confirmation is open denies token cleanly before free.
    private func testDestroyWithPendingClipboardRequest() {
        let view = addCommandTab(name: "clipclose", command: "/bin/sh -c 'echo CLIPCLOSE; sleep 30'", waitText: "CLIPCLOSE")
        let pb = NSPasteboard.general
        pb.clearContents()
        pb.setString("lineA\nlineB\n", forType: .string)
        view.bindingAction("paste_from_clipboard")
        check(waitFor(2.0, { self.drop.window.attachedSheet != nil }), "expected unsafe paste sheet on clipclose tab")
        drop.close(view)
        pump(0.2)
        check(drop.window.attachedSheet == nil, "closing tab should dismiss its clipboard sheet")
        check((drop.debugState()["prompts"] as? Int) == 0, "prompts should be 0 after closing tab")
        pass("11. surface destroy cleans up pending clipboard request & sheet")
    }
}

@main
struct MacOSTestMain {
    static func main() {
        guard GhosttyFFI.initGlobal(args: ["ghostdrop-macos-test"]) else {
            fputs("FAIL: GhosttyFFI.initGlobal failed\n", stderr)
            exit(1)
        }
        let app = NSApplication.shared
        let runner = MacOSTestRunner()
        app.delegate = runner
        app.setActivationPolicy(.accessory)
        app.run()
    }
}
