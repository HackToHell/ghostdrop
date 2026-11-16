import AppKit

protocol TabBarDelegate: AnyObject {
    func tabBarSelect(_ index: Int)
    func tabBarClose(_ index: Int)
    func tabBarMove(from: Int, to: Int)
    func tabBarNewTab()
}

/// A lightweight, custom-drawn tab bar. No NSTabView / SwiftUI: a single view
/// that draws text, so it costs next to nothing to update.
final class TabBarView: NSView {
    struct Item {
        var title: String
        var bell: Bool
    }

    weak var delegate: TabBarDelegate?
    var items: [Item] = [] { didSet { needsDisplay = true } }
    var selected = 0 { didSet { needsDisplay = true } }
    var background: NSColor = .black { didSet { needsDisplay = true } }
    var foreground: NSColor = .white { didSet { needsDisplay = true } }
    var bgAlpha: CGFloat = 1 { didSet { needsDisplay = true } }

    static let height: CGFloat = 26
    private let plusWidth: CGFloat = 28

    /// What the pointer is over.
    private enum Hover: Equatable {
        case tab(Int, onClose: Bool)
        case plus
    }
    private var hover: Hover?
    private var dragFrom: Int?
    private var dragStartX: CGFloat = 0
    private var tracking: NSTrackingArea?

    override var isFlipped: Bool { true }
    override var mouseDownCanMoveWindow: Bool { false }
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }

    // MARK: Geometry

    private func tabRects() -> [NSRect] {
        guard !items.isEmpty else { return [] }
        let avail = bounds.width - plusWidth
        let w = max(60, floor(avail / CGFloat(items.count)))
        return items.indices.map { NSRect(x: CGFloat($0) * w, y: 0, width: w, height: bounds.height) }
    }

    private var plusRect: NSRect {
        NSRect(x: bounds.width - plusWidth, y: 0, width: plusWidth, height: bounds.height)
    }

    private func closeRect(_ r: NSRect) -> NSRect {
        NSRect(x: r.maxX - 22, y: r.midY - 8, width: 16, height: 16)
    }

    private func index(at p: NSPoint) -> Int? {
        tabRects().firstIndex { $0.contains(p) }
    }

    // MARK: Drawing

    private func mix(_ a: NSColor, _ b: NSColor, _ t: CGFloat) -> NSColor {
        let a = a.usingColorSpace(.sRGB) ?? a, b = b.usingColorSpace(.sRGB) ?? b
        return NSColor(srgbRed: a.redComponent + (b.redComponent - a.redComponent) * t,
                       green: a.greenComponent + (b.greenComponent - a.greenComponent) * t,
                       blue: a.blueComponent + (b.blueComponent - a.blueComponent) * t,
                       alpha: 1)
    }

    override func draw(_ dirtyRect: NSRect) {
        let barBg = mix(background, foreground, 0.08).withAlphaComponent(bgAlpha)
        barBg.setFill()
        bounds.fill()

        let para = NSMutableParagraphStyle()
        para.lineBreakMode = .byTruncatingMiddle
        para.alignment = .center
        let font = NSFont.systemFont(ofSize: 12, weight: .medium)

        for (i, r) in tabRects().enumerated() {
            let isSel = i == selected
            if isSel {
                background.withAlphaComponent(bgAlpha).setFill()
                r.fill()
                // Accent line.
                foreground.withAlphaComponent(0.55).setFill()
                NSRect(x: r.minX, y: 0, width: r.width, height: 2).fill()
            } else if case .tab(i, _) = hover {
                mix(background, foreground, 0.13).withAlphaComponent(bgAlpha).setFill()
                r.fill()
            }
            // Separator.
            if i > 0 && !isSel && i - 1 != selected {
                foreground.withAlphaComponent(0.12).setFill()
                NSRect(x: r.minX, y: 6, width: 1, height: r.height - 12).fill()
            }

            let item = items[i]
            var title = item.title
            if item.bell { title = "🔔 " + title }
            if i < 9 { title = "\(i + 1)  " + title }
            let color = foreground.withAlphaComponent(isSel ? 0.95 : 0.55)
            let attrs: [NSAttributedString.Key: Any] = [.font: font, .foregroundColor: color, .paragraphStyle: para]
            let textRect = r.insetBy(dx: 26, dy: 0)
            let h = font.ascender - font.descender
            title.draw(in: NSRect(x: textRect.minX, y: r.midY - h / 2 - 1, width: textRect.width, height: h + 2),
                       withAttributes: attrs)

            if case .tab(i, let onClose) = hover {
                let cr = closeRect(r)
                if onClose {
                    foreground.withAlphaComponent(0.15).setFill()
                    NSBezierPath(roundedRect: cr, xRadius: 3, yRadius: 3).fill()
                }
                drawX(in: cr.insetBy(dx: 4.5, dy: 4.5), color: foreground.withAlphaComponent(0.7))
            }
        }

        // "+" button.
        let pr = plusRect
        let c = foreground.withAlphaComponent(hover == .plus ? 0.9 : 0.5)
        c.setStroke()
        let p = NSBezierPath()
        p.lineWidth = 1.3
        p.move(to: NSPoint(x: pr.midX - 5, y: pr.midY)); p.line(to: NSPoint(x: pr.midX + 5, y: pr.midY))
        p.move(to: NSPoint(x: pr.midX, y: pr.midY - 5)); p.line(to: NSPoint(x: pr.midX, y: pr.midY + 5))
        p.stroke()
    }

    private func drawX(in r: NSRect, color: NSColor) {
        color.setStroke()
        let p = NSBezierPath()
        p.lineWidth = 1.3
        p.move(to: NSPoint(x: r.minX, y: r.minY)); p.line(to: NSPoint(x: r.maxX, y: r.maxY))
        p.move(to: NSPoint(x: r.maxX, y: r.minY)); p.line(to: NSPoint(x: r.minX, y: r.maxY))
        p.stroke()
    }

    // MARK: Mouse

    override func updateTrackingAreas() {
        if let tracking { removeTrackingArea(tracking) }
        let t = NSTrackingArea(rect: bounds, options: [.mouseMoved, .mouseEnteredAndExited, .activeAlways, .inVisibleRect],
                               owner: self, userInfo: nil)
        addTrackingArea(t)
        tracking = t
        super.updateTrackingAreas()
    }

    override func mouseMoved(with event: NSEvent) {
        let p = convert(event.locationInWindow, from: nil)
        let h: Hover?
        if plusRect.contains(p) {
            h = .plus
        } else if let i = index(at: p) {
            h = .tab(i, onClose: closeRect(tabRects()[i]).contains(p))
        } else {
            h = nil
        }
        if h != hover {
            hover = h
            needsDisplay = true
        }
    }

    override func mouseExited(with event: NSEvent) {
        hover = nil
        needsDisplay = true
    }

    override func mouseDown(with event: NSEvent) {
        let p = convert(event.locationInWindow, from: nil)
        if plusRect.contains(p) {
            delegate?.tabBarNewTab()
            return
        }
        guard let i = index(at: p) else {
            if event.clickCount == 2 { delegate?.tabBarNewTab() }
            return
        }
        if closeRect(tabRects()[i]).contains(p) {
            delegate?.tabBarClose(i)
            return
        }
        delegate?.tabBarSelect(i)
        dragFrom = i
        dragStartX = p.x
    }

    override func mouseDragged(with event: NSEvent) {
        guard let from = dragFrom else { return }
        let p = convert(event.locationInWindow, from: nil)
        guard abs(p.x - dragStartX) > 8, let to = index(at: p), to != from else { return }
        delegate?.tabBarMove(from: from, to: to)
        dragFrom = to
    }

    override func mouseUp(with event: NSEvent) { dragFrom = nil }

    override func otherMouseUp(with event: NSEvent) {
        // Middle-click closes.
        guard event.buttonNumber == 2 else { return }
        let p = convert(event.locationInWindow, from: nil)
        if let i = index(at: p) { delegate?.tabBarClose(i) }
    }
}
