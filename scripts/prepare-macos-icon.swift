// Prepare the build host's macOS icon without changing the original artwork.
import AppKit
import Foundation
import ImageIO

guard CommandLine.arguments.count == 3 else {
    fputs("Usage: swift scripts/prepare-macos-icon.swift SOURCE_PNG OUTPUT_PNG\n", stderr)
    exit(2)
}
let input = URL(fileURLWithPath: CommandLine.arguments[1])
let output = URL(fileURLWithPath: CommandLine.arguments[2])
guard let source = CGImageSourceCreateWithURL(input as CFURL, nil),
      let image = CGImageSourceCreateImageAtIndex(source, 0, nil),
      image.width == image.height else {
    fputs("App icon must be a readable square image\n", stderr)
    exit(1)
}
if ProcessInfo.processInfo.operatingSystemVersion.majorVersion < 26 {
    try Data(contentsOf: input).write(to: output, options: .atomic)
    exit(0)
}

// A full opaque base avoids the extra plate applied to transparent legacy icons.
// macOS supplies the rounded mask; the navy fill matches the artwork's background.
guard let space = CGColorSpace(name: CGColorSpace.sRGB),
      let context = CGContext(data: nil, width: 1024, height: 1024, bitsPerComponent: 8,
                              bytesPerRow: 1024 * 4, space: space,
                              bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue) else {
    fputs("Cannot create macOS icon canvas\n", stderr)
    exit(1)
}
context.setFillColor(red: 18.0 / 255, green: 26.0 / 255, blue: 61.0 / 255, alpha: 1)
context.fill(CGRect(x: 0, y: 0, width: 1024, height: 1024))
context.interpolationQuality = .high
context.draw(image, in: CGRect(x: 0, y: 0, width: 1024, height: 1024))
guard let result = context.makeImage(),
      let png = NSBitmapImageRep(cgImage: result).representation(using: .png, properties: [:]) else {
    fputs("Cannot encode macOS icon PNG\n", stderr)
    exit(1)
}
try png.write(to: output, options: .atomic)
