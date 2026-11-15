import AppKit
import IOSurface

/// Debug helper: renders the window (tab bar via AppKit + terminal via the
/// IOSurface libghostty renders into) to a PNG, without needing Screen
/// Recording permission.
enum Snapshot {
    static func write(window: NSWindow, to path: String) {
        guard let content = window.contentView else { return }
        let scale = window.backingScaleFactor
        let size = content.bounds.size
        let w = Int(size.width * scale), h = Int(size.height * scale)
        guard w > 0, h > 0,
              let ctx = CGContext(data: nil, width: w, height: h, bitsPerComponent: 8, bytesPerRow: 0,
                                  space: CGColorSpace(name: CGColorSpace.sRGB)!,
                                  bitmapInfo: CGImageAlphaInfo.premultipliedFirst.rawValue | CGBitmapInfo.byteOrder32Little.rawValue)
        else { return }

        ctx.setFillColor(window.backgroundColor.cgColor)
        ctx.fill(CGRect(x: 0, y: 0, width: w, height: h))

        // AppKit-drawn views (tab bar, search bar).
        if let rep = content.bitmapImageRepForCachingDisplay(in: content.bounds) {
            content.cacheDisplay(in: content.bounds, to: rep)
            if let img = rep.cgImage { ctx.draw(img, in: CGRect(x: 0, y: 0, width: w, height: h)) }
        }

        // Terminal surfaces.
        for v in allSubviews(content) where v is SurfaceView && !v.isHidden {
            guard let obj = v.layer?.contents, CFGetTypeID(obj as CFTypeRef) == IOSurfaceGetTypeID() else { continue }
            let surface = obj as! IOSurface
            guard let img = image(from: surface) else { continue }
            let r = v.convert(v.bounds, to: content)
            ctx.draw(img, in: CGRect(x: r.minX * scale, y: r.minY * scale, width: r.width * scale, height: r.height * scale))
        }

        // Overlays (e.g. search bar) that sit above the terminal surfaces.
        for v in allSubviews(content) where !(v is SurfaceView) && !v.isHidden && v.superview?.subviews.contains(where: { $0 is SurfaceView }) == true {
            guard let rep = v.bitmapImageRepForCachingDisplay(in: v.bounds) else { continue }
            v.cacheDisplay(in: v.bounds, to: rep)
            let r = v.convert(v.bounds, to: content)
            if let img = rep.cgImage {
                ctx.draw(img, in: CGRect(x: r.minX * scale, y: r.minY * scale, width: r.width * scale, height: r.height * scale))
            }
        }

        guard let out = ctx.makeImage() else { return }
        let rep = NSBitmapImageRep(cgImage: out)
        try? rep.representation(using: .png, properties: [:])?.write(to: URL(fileURLWithPath: path))
    }

    private static func allSubviews(_ v: NSView) -> [NSView] {
        v.subviews + v.subviews.flatMap { allSubviews($0) }
    }

    private static func image(from s: IOSurface) -> CGImage? {
        s.lock(options: .readOnly, seed: nil)
        defer { s.unlock(options: .readOnly, seed: nil) }
        let w = s.width, h = s.height, bpr = s.bytesPerRow
        let data = Data(bytes: s.baseAddress, count: bpr * h)
        guard let provider = CGDataProvider(data: data as CFData) else { return nil }
        return CGImage(width: w, height: h, bitsPerComponent: 8, bitsPerPixel: 32, bytesPerRow: bpr,
                       space: CGColorSpace(name: CGColorSpace.sRGB)!,
                       bitmapInfo: CGBitmapInfo(rawValue: CGImageAlphaInfo.premultipliedFirst.rawValue | CGBitmapInfo.byteOrder32Little.rawValue),
                       provider: provider, decode: nil, shouldInterpolate: false, intent: .defaultIntent)
    }
}
