import AppKit
import Carbon.HIToolbox

/// System-wide hotkey via Carbon's RegisterEventHotKey. Unlike an event tap this
/// needs no Accessibility permission and costs nothing until the key is pressed.
final class GlobalHotkey {
    private var ref: EventHotKeyRef?
    private var handler: EventHandlerRef?
    var onPress: (() -> Void)?
    private(set) var registered: KeyCombo?

    init() {
        var spec = EventTypeSpec(eventClass: OSType(kEventClassKeyboard), eventKind: UInt32(kEventHotKeyPressed))
        let ud = Unmanaged.passUnretained(self).toOpaque()
        InstallEventHandler(GetApplicationEventTarget(), { _, _, ud in
            guard let ud else { return OSStatus(eventNotHandledErr) }
            let me = Unmanaged<GlobalHotkey>.fromOpaque(ud).takeUnretainedValue()
            me.onPress?()
            return noErr
        }, 1, &spec, ud, &handler)
    }

    /// Returns nil on success, or an error description.
    func register(_ combo: KeyCombo?) -> String? {
        unregister()
        guard let combo else { return nil }
        guard let code = combo.carbonKeyCode else { return "hotkey '\(combo)' has no key code" }
        let id = EventHotKeyID(signature: OSType(0x4744_5250), id: 1) // 'GDRP'
        let status = RegisterEventHotKey(code, combo.carbonModifiers, id, GetApplicationEventTarget(), 0, &ref)
        guard status == noErr else {
            ref = nil
            if status == eventHotKeyExistsErr {
                return "hotkey \(combo) is already taken by another app (e.g. iTerm2's hotkey window)"
            }
            return "could not register hotkey \(combo) (error \(status))"
        }
        registered = combo
        return nil
    }

    func unregister() {
        if let ref { UnregisterEventHotKey(ref) }
        ref = nil
        registered = nil
    }
}

/// Tiny IPC over distributed notifications: `ghostdrop ctl toggle|show|hide|new-tab [profile]|reload`.
enum Control {
    static let name = Notification.Name("com.ghostdrop.control")

    static func send(_ args: [String]) -> Int32 {
        guard let cmd = args.first else {
            FileHandle.standardError.write("usage: ghostdrop ctl toggle|show|hide|new-tab [profile]|next-tab|previous-tab|reload|login-item on|off\n".data(using: .utf8)!)
            return 2
        }
        var info: [String: String] = [:]
        if args.count > 1 { info["arg"] = args.dropFirst().joined(separator: " ") }
        DistributedNotificationCenter.default().postNotificationName(
            name, object: cmd, userInfo: info, deliverImmediately: true)
        return 0
    }
}
