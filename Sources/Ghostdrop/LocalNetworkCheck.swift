import AppKit
import Network

/// macOS (15+) "Local Network" privacy gates LAN / Bonjour / link-local access
/// for an app *and its child processes* (ssh, curl, … in our tabs). The consent
/// prompt is only shown reliably when the app itself touches the local network,
/// which a terminal never does — so without this, access silently stays denied.
///
/// A short Bonjour browse triggers the system prompt when the decision is still
/// pending, and tells us if access is denied so we can point the user to Settings.
final class LocalNetworkCheck {
    enum Status { case unknown, allowed, denied }

    private(set) var status: Status = .unknown
    var onChange: ((Status) -> Void)?
    /// The probe in flight (both nil when idle). Callbacks from an earlier
    /// probe check they're still the current one before reporting.
    private var browser: NWBrowser?
    private var timer: Timer?

    static let settingsURL = URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_LocalNetwork")!

    /// Probes again (restarting a probe in flight).
    func run(timeout: TimeInterval = 4) {
        stop()
        let params = NWParameters()
        params.includePeerToPeer = false
        let b = NWBrowser(for: .bonjour(type: "_ssh._tcp", domain: "local."), using: params)
        b.stateUpdateHandler = { [weak self, weak b] state in
            guard let self, let b, self.browser === b else { return }
            switch state {
            case .waiting(let error), .failed(let error):
                if case .dns(let code) = error, code == DNSServiceErrorType(kDNSServiceErr_PolicyDenied) {
                    self.finish(.denied)
                }
            default:
                break
            }
        }
        b.browseResultsChangedHandler = { [weak self, weak b] results, _ in
            guard let self, let b, self.browser === b else { return }
            if !results.isEmpty { self.finish(.allowed) }
        }
        browser = b
        b.start(queue: .main)
        // No denial within the timeout: assume allowed (or nothing to find).
        timer = Timer.scheduledTimer(withTimeInterval: timeout, repeats: false) { [weak self, weak b] _ in
            guard let self, let b, self.browser === b else { return }
            self.finish(.allowed)
        }
    }

    private func stop() {
        timer?.invalidate()
        timer = nil
        browser?.cancel()
        browser = nil
    }

    private func finish(_ s: Status) {
        stop()
        guard s != status else { return }
        status = s
        onChange?(s)
    }

    static func openSettings() {
        NSWorkspace.shared.open(settingsURL)
    }
}
