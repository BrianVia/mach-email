// Draws Mach's app icon into an .iconset directory for iconutil.
// Usage: swift make-icon.swift <out.iconset>
import AppKit

let out = URL(fileURLWithPath: CommandLine.arguments[1])
try FileManager.default.createDirectory(at: out, withIntermediateDirectories: true)

/// Big Sur icon grid on a 1024 canvas: an 824pt rounded square, centered,
/// with a soft drop shadow.
func draw(in size: CGFloat) {
    let s = size / 1024
    let body = NSRect(x: 100 * s, y: 100 * s, width: 824 * s, height: 824 * s)
    let squircle = NSBezierPath(roundedRect: body, xRadius: 185 * s, yRadius: 185 * s)

    NSGraphicsContext.saveGraphicsState()
    let shadow = NSShadow()
    shadow.shadowColor = NSColor.black.withAlphaComponent(0.3)
    shadow.shadowOffset = NSSize(width: 0, height: -10 * s)
    shadow.shadowBlurRadius = 24 * s
    shadow.set()
    NSColor.black.setFill()
    squircle.fill()
    NSGraphicsContext.restoreGraphicsState()

    NSGradient(
        starting: NSColor(srgbRed: 0.33, green: 0.55, blue: 1.0, alpha: 1),
        ending: NSColor(srgbRed: 0.16, green: 0.24, blue: 0.78, alpha: 1))!
        .draw(in: squircle, angle: -90)

    // Envelope, shifted right to leave room for the speed lines.
    let envelope = NSRect(x: 330 * s, y: 330 * s, width: 450 * s, height: 320 * s)
    let paper = NSBezierPath(roundedRect: envelope, xRadius: 44 * s, yRadius: 44 * s)
    NSColor.white.setFill()
    paper.fill()

    let flap = NSBezierPath()
    flap.move(to: NSPoint(x: envelope.minX + 34 * s, y: envelope.maxY - 30 * s))
    flap.line(to: NSPoint(x: envelope.midX, y: envelope.midY - 20 * s))
    flap.line(to: NSPoint(x: envelope.maxX - 34 * s, y: envelope.maxY - 30 * s))
    flap.lineWidth = 30 * s
    flap.lineCapStyle = .round
    flap.lineJoinStyle = .round
    NSColor(srgbRed: 0.22, green: 0.36, blue: 0.9, alpha: 1).setStroke()
    flap.stroke()

    NSColor.white.withAlphaComponent(0.9).setStroke()
    for (y, start) in [(560.0, 200.0), (490.0, 160.0), (420.0, 220.0)] {
        let line = NSBezierPath()
        line.move(to: NSPoint(x: start * s, y: y * s))
        line.line(to: NSPoint(x: 290 * s, y: y * s))
        line.lineWidth = 30 * s
        line.lineCapStyle = .round
        line.stroke()
    }
}

for points in [16, 32, 128, 256, 512] {
    for scale in [1, 2] {
        let pixels = points * scale
        let rep = NSBitmapImageRep(
            bitmapDataPlanes: nil, pixelsWide: pixels, pixelsHigh: pixels, bitsPerSample: 8, samplesPerPixel: 4,
            hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
        NSGraphicsContext.saveGraphicsState()
        NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
        draw(in: CGFloat(pixels))
        NSGraphicsContext.restoreGraphicsState()
        let name = scale == 1 ? "icon_\(points)x\(points).png" : "icon_\(points)x\(points)@2x.png"
        try rep.representation(using: .png, properties: [:])!.write(to: out.appendingPathComponent(name))
    }
}
