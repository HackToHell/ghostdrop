import AppKit
import Carbon.HIToolbox

// MARK: - Key combos

/// A keyboard shortcut such as `ctrl+shift+t` or `ctrl+grave_accent`.
///
/// Single characters are matched against the *unmodified* character the
/// current keyboard layout produces (so `ctrl+shift+t` works on any Latin
/// layout); named keys (`tab`, `page_up`, `grave_accent`, ...) are matched by
/// their physical key code.
struct KeyCombo: CustomStringConvertible {
    enum Key: Equatable {
        case char(String)
        case code(UInt16)
    }

    static let relevantMods: NSEvent.ModifierFlags = [.command, .control, .option, .shift]

    var mods: NSEvent.ModifierFlags
    var key: Key

    var description: String {
        var parts: [String] = []
        if mods.contains(.control) { parts.append("ctrl") }
        if mods.contains(.option) { parts.append("alt") }
        if mods.contains(.shift) { parts.append("shift") }
        if mods.contains(.command) { parts.append("cmd") }
        switch key {
        case .char(let c): parts.append(c)
        case .code(let c): parts.append(KeyCodes.name(for: c) ?? "keycode:\(c)")
        }
        return parts.joined(separator: "+")
    }

    static func parse(_ raw: String) -> KeyCombo? {
        let s = raw.trimmingCharacters(in: .whitespaces).lowercased()
        guard !s.isEmpty else { return nil }

        // Split on "+", but allow the key itself to be "+" (e.g. "cmd++").
        var parts = s.split(separator: "+", omittingEmptySubsequences: false).map(String.init)
        if s.hasSuffix("++") {
            parts.removeLast(2)
            parts.append("+")
        }
        guard let keyName = parts.popLast(), !keyName.isEmpty else { return nil }

        var mods: NSEvent.ModifierFlags = []
        for p in parts {
            switch p {
            case "ctrl", "control": mods.insert(.control)
            case "shift": mods.insert(.shift)
            case "alt", "opt", "option": mods.insert(.option)
            case "cmd", "command", "super": mods.insert(.command)
            default: return nil
            }
        }

        if let code = KeyCodes.named[keyName] {
            return KeyCombo(mods: mods, key: .code(code))
        }
        if keyName.count == 1 {
            return KeyCombo(mods: mods, key: .char(keyName))
        }
        return nil
    }

    func matches(_ event: NSEvent) -> Bool {
        guard event.type == .keyDown else { return false }
        guard event.modifierFlags.intersection(Self.relevantMods) == mods else { return false }
        switch key {
        case .code(let c):
            return event.keyCode == c
        case .char(let ch):
            if let s = event.characters(byApplyingModifiers: [])?.lowercased(), !s.isEmpty {
                if s == ch { return true }
                // A Latin layout produced a different character: not us.
                if s.unicodeScalars.allSatisfy({ $0.isASCII }) { return false }
            }
            // Non-Latin layout: fall back to the US physical position.
            return KeyCodes.usChars[ch] == event.keyCode
        }
    }

    /// Virtual key code for registering a global (Carbon) hotkey.
    var carbonKeyCode: UInt32? {
        switch key {
        case .code(let c): return UInt32(c)
        case .char(let ch): return KeyCodes.usChars[ch].map(UInt32.init)
        }
    }

    var carbonModifiers: UInt32 {
        var m: UInt32 = 0
        if mods.contains(.command) { m |= UInt32(cmdKey) }
        if mods.contains(.shift) { m |= UInt32(shiftKey) }
        if mods.contains(.option) { m |= UInt32(optionKey) }
        if mods.contains(.control) { m |= UInt32(controlKey) }
        return m
    }
}

enum KeyCodes {
    /// US-ANSI physical positions for printable characters.
    static let usChars: [String: UInt16] = [
        "a": 0, "s": 1, "d": 2, "f": 3, "h": 4, "g": 5, "z": 6, "x": 7, "c": 8, "v": 9,
        "b": 11, "q": 12, "w": 13, "e": 14, "r": 15, "y": 16, "t": 17,
        "1": 18, "2": 19, "3": 20, "4": 21, "6": 22, "5": 23, "=": 24, "9": 25, "7": 26,
        "-": 27, "8": 28, "0": 29, "]": 30, "o": 31, "u": 32, "[": 33, "i": 34, "p": 35,
        "l": 37, "j": 38, "'": 39, "k": 40, ";": 41, "\\": 42, ",": 43, "/": 44,
        "n": 45, "m": 46, ".": 47, "`": 50, " ": 49,
    ]

    /// Named (physical) keys. Names follow Ghostty's keybind syntax where possible.
    static let named: [String: UInt16] = {
        var m: [String: UInt16] = [
            "grave_accent": 50, "backquote": 50, "grave": 50, "backtick": 50,
            "minus": 27, "equal": 24, "bracket_left": 33, "bracket_right": 30,
            "backslash": 42, "semicolon": 41, "quote": 39, "apostrophe": 39,
            "comma": 43, "period": 47, "slash": 44,
            "space": 49, "tab": 48, "enter": 36, "return": 36, "escape": 53, "esc": 53,
            "backspace": 51, "delete": 117, "insert": 114, "help": 114,
            "home": 115, "end": 119, "page_up": 116, "pageup": 116, "page_down": 121, "pagedown": 121,
            "arrow_left": 123, "left": 123, "arrow_right": 124, "right": 124,
            "arrow_down": 125, "down": 125, "arrow_up": 126, "up": 126,
            "digit_1": 18, "digit_2": 19, "digit_3": 20, "digit_4": 21, "digit_5": 23,
            "digit_6": 22, "digit_7": 26, "digit_8": 28, "digit_9": 25, "digit_0": 29,
        ]
        let fkeys: [UInt16] = [122, 120, 99, 118, 96, 97, 98, 100, 101, 109, 103, 111,
                               105, 107, 113, 106, 64, 79, 80, 90]
        for (i, c) in fkeys.enumerated() { m["f\(i + 1)"] = c }
        return m
    }()

    static func name(for code: UInt16) -> String? {
        let preferred = ["grave_accent", "tab", "enter", "escape", "space", "backspace", "delete",
                         "home", "end", "page_up", "page_down", "left", "right", "up", "down"]
        for p in preferred where named[p] == code { return p }
        return named.first(where: { $0.value == code })?.key
    }
}

// MARK: - Actions

enum DropAction: CustomStringConvertible {
    var description: String {
        switch self {
        case .newTab(let p): return p.map { "new_tab:\($0)" } ?? "new_tab"
        case .closeTab: return "close_tab"
        case .nextTab: return "next_tab"
        case .previousTab: return "previous_tab"
        case .lastTab: return "last_tab"
        case .gotoTab(let n): return "goto_tab:\(n)"
        case .moveTab(let n): return "move_tab:\(n)"
        case .toggle: return "toggle"
        case .hide: return "hide"
        case .renameTab: return "rename_tab"
        case .reloadConfig: return "reload_config"
        case .openConfig: return "open_config"
        case .quit: return "quit"
        case .unbind: return "unbind"
        case .ghostty(let a): return "ghostty:\(a)"
        }
    }

    case newTab(profile: String?)
    case closeTab
    case nextTab, previousTab, lastTab
    case gotoTab(Int)
    case moveTab(Int)
    case toggle, hide
    case renameTab
    case reloadConfig, openConfig
    case quit
    case unbind
    /// Any Ghostty binding action, e.g. `ghostty:clear_screen`.
    case ghostty(String)

    static func parse(_ raw: String) -> DropAction? {
        let s = raw.trimmingCharacters(in: .whitespaces)
        let (name, arg): (String, String?) = {
            guard let i = s.firstIndex(of: ":") else { return (s, nil) }
            return (String(s[..<i]), String(s[s.index(after: i)...]))
        }()
        switch name {
        case "new_tab", "new_window": return .newTab(profile: arg.flatMap { $0.isEmpty ? nil : $0 })
        case "close_tab", "close_surface": return .closeTab
        case "next_tab": return .nextTab
        case "previous_tab", "prev_tab": return .previousTab
        case "last_tab": return .lastTab
        case "goto_tab": return arg.flatMap(Int.init).map { .gotoTab($0) }
        case "move_tab": return arg.flatMap(Int.init).map { .moveTab($0) }
        case "toggle", "toggle_quick_terminal", "toggle_visibility": return .toggle
        case "hide": return .hide
        case "rename_tab", "prompt_surface_title", "prompt_tab_title": return .renameTab
        case "reload_config": return .reloadConfig
        case "open_config": return .openConfig
        case "quit": return .quit
        case "unbind": return .unbind
        case "ghostty": return arg.map { .ghostty($0) }
        default: return nil
        }
    }
}

// MARK: - Profiles

struct Profile {
    var name: String
    /// Command run through `/bin/sh -c`. nil/empty = Ghostty's `command` or the login shell.
    var command: String?
    var workingDirectory: String?
    var env: [String: String] = [:]
    var title: String?
    /// Override $TERM for this profile's command (e.g. xterm-256color for hosts
    /// without Ghostty's terminfo).
    var term: String?
    var waitAfterCommand = false

    /// The command actually handed to libghostty.
    var effectiveCommand: String? {
        guard let command, !command.isEmpty else { return nil }
        if let term { return "/usr/bin/env TERM=\(term) \(command)" }
        return command
    }
}

enum TabBarMode: String { case auto, always, never }
enum TabBarPosition: String { case top, bottom }

// MARK: - Settings

/// Ghostdrop's own configuration. It lives in `~/.config/ghostdrop/config` and uses
/// Ghostty's `key = value` syntax. Keys Ghostdrop doesn't know about are passed
/// straight through to libghostty, so fonts, themes, keybinds, etc. can be set
/// there and override the regular Ghostty config.
final class Settings {
    static var path: String {
        if let p = ProcessInfo.processInfo.environment["GHOSTDROP_CONFIG"], !p.isEmpty {
            return (p as NSString).expandingTildeInPath
        }
        return (NSHomeDirectory() as NSString).appendingPathComponent(".config/ghostdrop/config")
    }

    var hotkey: KeyCombo? = KeyCombo.parse("ctrl+grave_accent")
    static let builtinProfile = "shell"
    var profiles: [String: Profile] = [builtinProfile: Profile(name: builtinProfile)]
    var profileOrder: [String] = [builtinProfile]
    var defaultProfileName = builtinProfile
    var bindings: [(KeyCombo, DropAction)] = []
    var tabBarMode: TabBarMode = .auto
    var tabBarPosition: TabBarPosition = .top
    var statusItem = true
    var dockIcon = false
    var inheritGhosttyConfig = true
    var prespawn = true
    var controlCommands = false
    /// Lines that belong to libghostty.
    var ghosttyLines: [String] = []
    var errors: [String] = []

    static let defaultBindings: [(String, String)] = [
        ("ctrl+shift+t", "new_tab"),
        ("ctrl+page_up", "previous_tab"),
        ("ctrl+page_down", "next_tab"),
        ("ctrl+shift+page_up", "move_tab:-1"),
        ("ctrl+shift+page_down", "move_tab:1"),
        ("cmd+shift+left", "move_tab:-1"),
        ("cmd+shift+right", "move_tab:1"),
    ]

    var defaultProfile: Profile {
        profiles[defaultProfileName] ?? profiles[Self.builtinProfile] ?? Profile(name: Self.builtinProfile)
    }

    func profile(named name: String?) -> Profile {
        guard let name else { return defaultProfile }
        return profiles[name] ?? defaultProfile
    }

    static func load() -> Settings {
        let s = Settings()
        for (k, a) in defaultBindings {
            if let c = KeyCombo.parse(k), let act = DropAction.parse(a) { s.bindings.append((c, act)) }
        }
        let path = Settings.path
        if !FileManager.default.fileExists(atPath: path) {
            Settings.writeTemplate(to: path)
        }
        guard let text = try? String(contentsOfFile: path, encoding: .utf8) else { return s }
        s.parse(text)
        if s.profiles[s.defaultProfileName] == nil {
            s.errors.append("default-profile '\(s.defaultProfileName)' is not defined; using '\(builtinProfile)'")
            s.defaultProfileName = builtinProfile
        }
        return s
    }

    private func profileRef(_ name: String) -> String {
        if profiles[name] == nil {
            profiles[name] = Profile(name: name)
            profileOrder.append(name)
        }
        return name
    }

    func parse(_ text: String) {
        for (n, rawLine) in text.components(separatedBy: .newlines).enumerated() {
            let line = rawLine.trimmingCharacters(in: .whitespaces)
            if line.isEmpty || line.hasPrefix("#") { continue }
            guard let eq = line.firstIndex(of: "=") else {
                errors.append("line \(n + 1): expected key = value")
                continue
            }
            let key = line[..<eq].trimmingCharacters(in: .whitespaces)
            var value = line[line.index(after: eq)...].trimmingCharacters(in: .whitespaces)
            if value.count >= 2, value.hasPrefix("\""), value.hasSuffix("\"") {
                value = String(value.dropFirst().dropLast())
            }

            if key.hasPrefix("profile.") {
                let rest = key.dropFirst("profile.".count)
                guard let dot = rest.lastIndex(of: ".") else {
                    errors.append("line \(n + 1): expected profile.<name>.<field>")
                    continue
                }
                let name = profileRef(String(rest[..<dot]))
                let field = rest[rest.index(after: dot)...]
                switch field {
                case "command": profiles[name]!.command = value.isEmpty ? nil : value
                case "cwd", "working-directory":
                    profiles[name]!.workingDirectory = value.isEmpty ? nil : (value as NSString).expandingTildeInPath
                case "title": profiles[name]!.title = value.isEmpty ? nil : value
                case "term":
                    if value.isEmpty {
                        profiles[name]!.term = nil
                    } else if value.allSatisfy({ $0.isLetter || $0.isNumber || "-_.+".contains($0) }) {
                        profiles[name]!.term = value
                    } else {
                        errors.append("line \(n + 1): invalid term '\(value)'")
                    }
                case "wait-after-command": profiles[name]!.waitAfterCommand = Self.bool(value)
                case "env":
                    if let i = value.firstIndex(of: "=") {
                        profiles[name]!.env[String(value[..<i])] = String(value[value.index(after: i)...])
                    } else {
                        errors.append("line \(n + 1): env must be KEY=VALUE")
                    }
                default: errors.append("line \(n + 1): unknown profile field '\(field)'")
                }
                continue
            }

            switch key {
            case "hotkey":
                if value.isEmpty || value == "none" {
                    hotkey = nil
                } else if let c = KeyCombo.parse(value) {
                    hotkey = c
                } else {
                    errors.append("line \(n + 1): invalid hotkey '\(value)'")
                }
            case "default-profile":
                // Don't auto-create: a typo should be reported, not become an empty profile.
                defaultProfileName = value
            case "bind": parseBind(value, line: n + 1)
            case "tab-bar": if let v = TabBarMode(rawValue: value) { tabBarMode = v } else { errors.append("line \(n + 1): tab-bar must be auto|always|never") }
            case "tab-bar-position": if let v = TabBarPosition(rawValue: value) { tabBarPosition = v } else { errors.append("line \(n + 1): tab-bar-position must be top|bottom") }
            case "status-item": statusItem = Self.bool(value)
            case "dock-icon": dockIcon = Self.bool(value)
            case "inherit-ghostty-config": inheritGhosttyConfig = Self.bool(value)
            case "prespawn": prespawn = Self.bool(value)
            case "control-commands": controlCommands = Self.bool(value)
            default:
                ghosttyLines.append(line)
            }
        }
    }

    private func parseBind(_ value: String, line: Int) {
        if value == "clear" {
            bindings.removeAll()
            return
        }
        // Split "trigger=action" on the first '=' that isn't the key itself (e.g. "cmd+==...").
        let chars = Array(value)
        var split: Int?
        for i in chars.indices where chars[i] == "=" {
            if i == 0 || chars[i - 1] == "+" { continue }
            split = i
            break
        }
        guard let split else {
            errors.append("line \(line): bind must be trigger=action")
            return
        }
        let trig = String(chars[..<split])
        let act = String(chars[(split + 1)...])
        guard let combo = KeyCombo.parse(trig) else {
            errors.append("line \(line): invalid trigger '\(trig)'")
            return
        }
        guard let action = DropAction.parse(act) else {
            errors.append("line \(line): unknown action '\(act)'")
            return
        }
        bindings.removeAll { $0.0.mods == combo.mods && $0.0.key == combo.key }
        if case .unbind = action { return }
        bindings.append((combo, action))
    }

    static func bool(_ s: String) -> Bool {
        ["true", "yes", "1", "on"].contains(s.lowercased())
    }

    static func writeTemplate(to path: String) {
        let dir = (path as NSString).deletingLastPathComponent
        try? FileManager.default.createDirectory(atPath: dir, withIntermediateDirectories: true)
        try? Settings.template.write(toFile: path, atomically: true, encoding: .utf8)
    }

    static let template = """
    # Ghostdrop configuration.
    #
    # Ghostdrop reads your normal Ghostty config first (fonts, theme, keybinds...),
    # then this file. Any key that isn't a Ghostdrop key below is passed straight to
    # libghostty, so you can override Ghostty settings for the drop-down only.
    # Reload with cmd+shift+, (Ghostty's reload_config) or from the menu bar icon.

    # Global hotkey that shows/hides the terminal. Works without Accessibility
    # permission. Examples: ctrl+grave_accent, alt+space, cmd+f12
    # hotkey = ctrl+grave_accent

    # Geometry & behaviour use Ghostty's quick-terminal options:
    # (top | bottom | left | right | center)
    # quick-terminal-position = top
    # (or 50%,80% / 600px)
    # quick-terminal-size = 50%
    # (main | mouse | macos-menu-bar)
    # quick-terminal-screen = mouse
    # quick-terminal-animation-duration = 0.1
    # quick-terminal-autohide = true

    # Profiles: what a new tab runs. The built-in "shell" profile is your login shell.
    # profile.work.command = ssh my-host
    # ($TERM override (hosts without ghostty terminfo))
    # profile.work.term = xterm-256color
    # profile.work.env = FOO=bar
    # profile.work.cwd = ~/src
    # profile.work.title = work
    # (used at startup and by new_tab / cmd+t)
    # default-profile = shell

    # App-level key bindings (handled before the terminal sees the key).
    # Ghostty keybinds (cmd+t, cmd+w, ctrl+tab, cmd+1..9, ...) keep working too.
    # (new_tab:<profile> for a specific profile)
    # bind = ctrl+shift+t=new_tab
    # bind = ctrl+page_down=next_tab
    # bind = ctrl+page_up=previous_tab
    # bind = alt+1=goto_tab:1
    # bind = ctrl+shift+w=close_tab
    # (remove a default binding)
    # bind = ctrl+shift+t=unbind

    # (auto (hidden with one tab) | always | never)
    # tab-bar = auto
    # (top | bottom)
    # tab-bar-position = top
    # (menu bar icon)
    # status-item = true
    # dock-icon = false
    # (start the first tab at launch)
    # prespawn = true
    """
}
