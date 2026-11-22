// Draws the menu-bar status item glyph: the brand's open envelope with a feather rising out of it,
// reduced to a single-colour macOS template image (black + alpha; AppKit tints it for light/dark
// menu bars and the highlighted state).
//
// Output is a one-page vector PDF, 18 pt tall and as wide as the glyph, so it stays crisp at every
// backing scale. Overlaps are separated with CGPath boolean ops (macOS 13+) rather than blend modes, so the PDF
// contains plain fills only.
//
// Usage: swift scripts/render-menu-bar-icon.swift <output.pdf> [preview.png]
import AppKit
import CoreGraphics
import Foundation

guard CommandLine.arguments.count >= 2 else {
    fputs("Usage: swift scripts/render-menu-bar-icon.swift <output.pdf> [preview.png]\n", stderr)
    exit(1)
}
let outputPath = CommandLine.arguments[1]
let previewPath: String? = CommandLine.arguments.count >= 3 ? CommandLine.arguments[2] : nil

let line: CGFloat = 1.3 // stroke weight, close to SF Symbols "regular" at menu-bar size
let gap: CGFloat = 0.7 // knockout between overlapping layers

func stroked(_ path: CGPath, _ width: CGFloat = line) -> CGPath {
    path.copy(strokingWithWidth: width, lineCap: .round, lineJoin: .round, miterLimit: 4)
}

/// Grow a filled region by `amount` on every side.
func grown(_ path: CGPath, by amount: CGFloat) -> CGPath {
    path.union(stroked(path, amount * 2))
}

// MARK: Envelope (upright for crisp horizontals; the brand tilt does not survive 18 pt)

let body = CGRect(x: 0.6, y: 1.4, width: 13.8, height: 8.4)
let bodyOutline = CGPath(
    roundedRect: body.insetBy(dx: line / 2, dy: line / 2),
    cornerWidth: 1.5, cornerHeight: 1.5, transform: nil)

let inset = line / 2
let foldApex = CGPoint(x: body.midX, y: body.minY + 4.6)

// Open flap behind everything: a low triangle standing on the body's top edge.
let flap = CGMutablePath()
flap.move(to: CGPoint(x: body.minX + inset + 0.3, y: body.maxY - inset))
flap.addLine(to: CGPoint(x: body.midX, y: body.maxY + 3.3))
flap.addLine(to: CGPoint(x: body.maxX - inset - 0.3, y: body.maxY - inset))

// Front pocket: the fold lines from the bottom corners meeting in the middle.
let fold = CGMutablePath()
fold.move(to: CGPoint(x: body.minX + inset + 0.5, y: body.minY + inset + 0.4))
fold.addLine(to: foldApex)
fold.addLine(to: CGPoint(x: body.maxX - inset - 0.5, y: body.minY + inset + 0.4))

// Region the front pocket hides (everything below the fold lines, inside the body).
let pocket = CGMutablePath()
pocket.move(to: CGPoint(x: body.minX, y: body.minY))
pocket.addLine(to: CGPoint(x: body.minX, y: body.minY + 0.6))
pocket.addLine(to: foldApex)
pocket.addLine(to: CGPoint(x: body.maxX, y: body.minY + 0.6))
pocket.addLine(to: CGPoint(x: body.maxX, y: body.minY))
pocket.closeSubpath()

// MARK: Feather, drawn along +x in its own frame, then rotated into place

let featherLength: CGFloat = 16.0
let vaneStart: CGFloat = 5.4
let tip = CGPoint(x: featherLength, y: 0.5) // slight curl toward the upper edge

let vane = CGMutablePath()
vane.move(to: CGPoint(x: vaneStart, y: 0))
vane.addCurve(
    to: tip,
    control1: CGPoint(x: vaneStart + 1.6, y: 2.5),
    control2: CGPoint(x: featherLength - 2.6, y: 2.9))
vane.addCurve(
    to: CGPoint(x: vaneStart, y: 0),
    control1: CGPoint(x: featherLength - 1.6, y: -2.0),
    control2: CGPoint(x: vaneStart + 2.2, y: -2.2))
vane.closeSubpath()

// Two splits in the lower barbs, echoing the brand feather. Each wedge opens at the outer edge
// and narrows toward the shaft, angled back toward the base the way barbs part.
let notches = CGMutablePath()
for x in [CGFloat(9.3), CGFloat(12.3)] {
    notches.move(to: CGPoint(x: x, y: -3.4))
    notches.addLine(to: CGPoint(x: x + 1.3, y: -3.4))
    notches.addLine(to: CGPoint(x: x - 0.9, y: -0.75))
    notches.closeSubpath()
}

let shaftLine = CGMutablePath()
shaftLine.move(to: CGPoint(x: vaneStart - 0.2, y: 0))
shaftLine.addQuadCurve(
    to: CGPoint(x: featherLength - 2.2, y: 0.5),
    control: CGPoint(x: (vaneStart + featherLength) / 2, y: 0.05))
let quill = CGMutablePath()
quill.move(to: CGPoint(x: 0, y: 0))
quill.addLine(to: CGPoint(x: vaneStart + 0.4, y: 0))

let featherLocal = vane.subtracting(notches).subtracting(stroked(shaftLine, 0.65))
    .union(stroked(quill, 1.2))

let featherBase = CGPoint(x: 5.6, y: 3.6)
let featherAngle: CGFloat = 50 * .pi / 180
var place = CGAffineTransform(translationX: featherBase.x, y: featherBase.y)
    .rotated(by: featherAngle)
let featherPlaced = featherLocal.copy(using: &place)!

// MARK: Layering — back (body + flap) < feather < front pocket

let feather = featherPlaced.subtracting(grown(pocket, by: gap * 0.6))
let back = stroked(bodyOutline).union(stroked(flap))
let backVisible = back.subtracting(grown(feather, by: gap))
let drawn = backVisible.union(feather).union(stroked(fold))

// Scale the artwork to 16 pt tall (the usable height of a 22 pt menu bar) and centre it on an 18 pt
// canvas; width hugs the glyph so the status item stays tight.
let artHeight: CGFloat = 16
let raw = drawn.boundingBoxOfPath
let scale = artHeight / raw.height
let padX: CGFloat = 0.5
let canvas = CGRect(x: 0, y: 0, width: (raw.width * scale + padX * 2).rounded(.up), height: 18)
var fit = CGAffineTransform(
    translationX: (canvas.width - raw.width * scale) / 2, y: (canvas.height - artHeight) / 2)
    .scaledBy(x: scale, y: scale)
    .translatedBy(x: -raw.minX, y: -raw.minY)
let glyph = drawn.copy(using: &fit)!
print("canvas \(canvas.width)x\(canvas.height) pt, glyph \(glyph.boundingBoxOfPath.size) pt")

// MARK: Output

func writePDF(_ path: CGPath, to url: URL) {
    var box = canvas
    guard let context = CGContext(url as CFURL, mediaBox: &box, nil) else {
        fputs("Cannot create PDF at \(url.path)\n", stderr)
        exit(1)
    }
    context.beginPDFPage(nil)
    context.setFillColor(CGColor(gray: 0, alpha: 1))
    context.addPath(path)
    context.fillPath()
    context.endPDFPage()
    context.closePDF()
}

/// Preview sheet: glyph at 1x/2x on light and dark menu-bar colours plus a large proof.
func writePreview(_ path: CGPath, to url: URL) {
    let scales: [CGFloat] = [1, 2, 3, 12]
    let pad: CGFloat = 16
    let cells = scales.map { canvas.height * $0 }
    let width = cells.reduce(pad, { $0 + $1 + pad })
    let height = (cells.max()! + pad * 2) * 2
    let space = CGColorSpaceCreateDeviceRGB()
    guard let ctx = CGContext(
        data: nil, width: Int(width), height: Int(height), bitsPerComponent: 8, bytesPerRow: 0,
        space: space, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)
    else { exit(1) }
    let rows: [(bg: CGColor, fg: CGColor)] = [
        (CGColor(red: 0.93, green: 0.93, blue: 0.94, alpha: 1), CGColor(gray: 0.1, alpha: 1)),
        (CGColor(red: 0.16, green: 0.17, blue: 0.19, alpha: 1), CGColor(gray: 0.96, alpha: 1)),
    ]
    let rowHeight = height / 2
    for (i, row) in rows.enumerated() {
        let y0 = CGFloat(i) * rowHeight
        ctx.setFillColor(row.bg)
        ctx.fill(CGRect(x: 0, y: y0, width: width, height: rowHeight))
        var x = pad
        for (scale, cell) in zip(scales, cells) {
            ctx.saveGState()
            ctx.translateBy(x: x + (cell - canvas.width * scale) / 2, y: y0 + (rowHeight - cell) / 2)
            ctx.scaleBy(x: scale, y: scale)
            ctx.setFillColor(row.fg)
            ctx.addPath(path)
            ctx.fillPath()
            ctx.restoreGState()
            x += cell + pad
        }
    }
    let image = NSBitmapImageRep(cgImage: ctx.makeImage()!)
    try! image.representation(using: .png, properties: [:])!.write(to: url)
}

writePDF(glyph, to: URL(fileURLWithPath: outputPath))
if let previewPath {
    writePreview(glyph, to: URL(fileURLWithPath: previewPath))
}
