import AppKit

/// An invisible strip along a free edge of the drop-down that resizes it,
/// like dragging the bottom edge of iTerm2's hotkey window. Shows the resize
/// cursor and a subtle line on hover; double-click resets to the configured size.
final class ResizeHandle: NSView {
    enum Edge { case top, bottom, left, right }

    static let thickness: CGFloat = 5

    let edge: Edge
    weak var controller: DropController?
    var color: NSColor = .white { didSet { needsDisplay = true } }
    private var hovering = false { didSet { if oldValue != hovering { needsDisplay = true } } }
    private var dragging = false { didSet { if oldValue != dragging { needsDisplay = true } } }
    private var tracking: NSTrackingArea?

    init(edge: Edge) {
        self.edge = edge
        super.init(frame: .zero)
    }

    required init?(coder: NSCoder) { fatalError() }

    private var cursor: NSCursor {
        switch edge {
        case .top, .bottom: return .resizeUpDown
        case .left, .right: return .resizeLeftRight
        }
    }

    override var mouseDownCanMoveWindow: Bool { false }
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }

    override func updateTrackingAreas() {
        if let tracking { removeTrackingArea(tracking) }
        let t = NSTrackingArea(rect: bounds,
                               options: [.cursorUpdate, .mouseEnteredAndExited, .activeAlways, .inVisibleRect],
                               owner: self, userInfo: nil)
        addTrackingArea(t)
        tracking = t
        super.updateTrackingAreas()
    }

    override func resetCursorRects() { addCursorRect(bounds, cursor: cursor) }
    override func cursorUpdate(with event: NSEvent) { cursor.set() }
    override func mouseEntered(with event: NSEvent) { hovering = true; cursor.set() }
    override func mouseExited(with event: NSEvent) { hovering = false }

    override func mouseDown(with event: NSEvent) {
        if event.clickCount == 2 {
            controller?.resetSize()
            return
        }
        dragging = true
        controller?.beginResize(edge, at: NSEvent.mouseLocation)
    }

    override func mouseDragged(with event: NSEvent) {
        // Use the global cursor position: the window moves under the mouse while
        // resizing, so window-relative event locations would be stale.
        controller?.resize(to: NSEvent.mouseLocation)
    }

    override func mouseUp(with event: NSEvent) {
        dragging = false
        controller?.endResize()
    }

    override func draw(_ dirtyRect: NSRect) {
        guard hovering || dragging else { return }
        color.withAlphaComponent(dragging ? 0.5 : 0.3).setFill()
        let w: CGFloat = 2
        switch edge {
        case .bottom: NSRect(x: 0, y: 0, width: bounds.width, height: w).fill()
        case .top: NSRect(x: 0, y: bounds.height - w, width: bounds.width, height: w).fill()
        case .left: NSRect(x: 0, y: 0, width: w, height: bounds.height).fill()
        case .right: NSRect(x: bounds.width - w, y: 0, width: w, height: bounds.height).fill()
        }
    }
}
