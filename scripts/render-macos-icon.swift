import AppKit
import CoreGraphics
import Foundation

guard CommandLine.arguments.count >= 3 else {
    fputs("Usage: swift scripts/render-macos-icon.swift <input.png> <output_1024.png> [optional.icns]\n", stderr)
    exit(1)
}

let inputPath = CommandLine.arguments[1]
let outputPath = CommandLine.arguments[2]
let icnsPath: String? = CommandLine.arguments.count >= 4 ? CommandLine.arguments[3] : nil

guard let srcData = try? Data(contentsOf: URL(fileURLWithPath: inputPath)),
      let srcRep = NSBitmapImageRep(data: srcData),
      let srcCG = srcRep.cgImage else {
    fputs("Failed to load input image: \(inputPath)\n", stderr)
    exit(1)
}

/// Build a continuous-curvature superellipse (squircle) CGPath matching macOS Big Sur+ HIG
func macOSSquirclePath(in rect: CGRect, power: CGFloat = 5.0, steps: Int = 512) -> CGPath {
    let path = CGMutablePath()
    let a = rect.width / 2.0
    let b = rect.height / 2.0
    let cx = rect.midX
    let cy = rect.midY
    // Apple icon grid approximates superellipse |x/a|^n + |y/b|^n = 1 with n ≈ 4.5..5.0
    let exp = 2.0 / power
    for i in 0...steps {
        let t = (CGFloat(i) / CGFloat(steps)) * 2.0 * .pi
        let cosT = cos(t)
        let sinT = sin(t)
        let x = cx + a * (cosT >= 0 ? 1 : -1) * pow(abs(cosT), exp)
        let y = cy + b * (sinT >= 0 ? 1 : -1) * pow(abs(sinT), exp)
        if i == 0 {
            path.move(to: CGPoint(x: x, y: y))
        } else {
            path.addLine(to: CGPoint(x: x, y: y))
        }
    }
    path.closeSubpath()
    return path
}

func renderIcon(size: Int, src: CGImage) -> Data? {
    let fSize = CGFloat(size)
    let colorSpace = CGColorSpace(name: CGColorSpace.sRGB)!
    guard let ctx = CGContext(
        data: nil,
        width: size,
        height: size,
        bitsPerComponent: 8,
        bytesPerRow: size * 4,
        space: colorSpace,
        bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
    ) else {
        return nil
    }
    ctx.interpolationQuality = .high

    // Apple HIG 1024x1024 grid: 824x824 squircle body (inset = 100/1024 = 9.765625%)
    let inset = fSize * (100.0 / 1024.0)
    let bodySize = fSize - 2.0 * inset
    let bodyRect = CGRect(x: inset, y: inset, width: bodySize, height: bodySize)
    let squircle = macOSSquirclePath(in: bodyRect, power: 4.6)

    // 1. Ambient macOS drop shadow outside the squircle
    ctx.saveGState()
    let shadowOffset = CGSize(width: 0, height: -fSize * (12.0 / 1024.0))
    let shadowBlur = fSize * (26.0 / 1024.0)
    ctx.setShadow(
        offset: shadowOffset,
        blur: shadowBlur,
        color: CGColor(srgbRed: 0, green: 0, blue: 0, alpha: 0.30)
    )
    ctx.addPath(squircle)
    ctx.setFillColor(CGColor(srgbRed: 0, green: 0, blue: 0, alpha: 1.0))
    ctx.fillPath()
    ctx.restoreGState()

    // 2. Clip to squircle and draw artwork slightly overscanned (1.03x) to avoid edge fringe
    ctx.saveGState()
    ctx.addPath(squircle)
    ctx.clip()
    let overscan = bodySize * 0.02
    let drawRect = bodyRect.insetBy(dx: -overscan, dy: -overscan)
    ctx.draw(src, in: drawRect)

    // 3. Subtle inner rim highlight (top subtle white sheen, bottom subtle depth)
    ctx.addPath(squircle)
    ctx.setLineWidth(max(1.0, fSize * (3.0 / 1024.0)))
    ctx.setStrokeColor(CGColor(srgbRed: 1, green: 1, blue: 1, alpha: 0.16))
    ctx.strokePath()
    ctx.restoreGState()

    guard let outCG = ctx.makeImage() else { return nil }
    let rep = NSBitmapImageRep(cgImage: outCG)
    return rep.representation(using: .png, properties: [:])
}

guard let png1024 = renderIcon(size: 1024, src: srcCG) else {
    fputs("Failed to render 1024 icon\n", stderr)
    exit(1)
}
try png1024.write(to: URL(fileURLWithPath: outputPath))

if let icnsPath = icnsPath {
    let fm = FileManager.default
    let tmpDir = URL(fileURLWithPath: NSTemporaryDirectory())
        .appendingPathComponent("featherpost_\(UUID().uuidString).iconset")
    try fm.createDirectory(at: tmpDir, withIntermediateDirectories: true)
    defer { try? fm.removeItem(at: tmpDir) }

    let specs: [(String, Int)] = [
        ("icon_16x16.png", 16),
        ("icon_16x16@2x.png", 32),
        ("icon_32x32.png", 32),
        ("icon_32x32@2x.png", 64),
        ("icon_128x128.png", 128),
        ("icon_128x128@2x.png", 256),
        ("icon_256x256.png", 256),
        ("icon_256x256@2x.png", 512),
        ("icon_512x512.png", 512),
        ("icon_512x512@2x.png", 1024),
    ]
    for (name, px) in specs {
        guard let data = renderIcon(size: px, src: srcCG) else { exit(1) }
        try data.write(to: tmpDir.appendingPathComponent(name))
    }

    let proc = Process()
    proc.executableURL = URL(fileURLWithPath: "/usr/bin/iconutil")
    proc.arguments = ["-c", "icns", tmpDir.path, "-o", icnsPath]
    try proc.run()
    proc.waitUntilExit()
    if proc.terminationStatus != 0 {
        fputs("iconutil failed with status \(proc.terminationStatus)\n", stderr)
        exit(1)
    }
}
