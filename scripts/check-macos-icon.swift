// Run: swift scripts/check-macos-icon.swift /absolute/SwitchX.app [snapshot.png]
// Check SwitchX's blue/purple artwork through the same native icon service used by Dock.
import AppKit
import Foundation

func artworkWidth(_ image: NSImage) throws -> Double {
    var rect = NSRect(x: 0, y: 0, width: 256, height: 256)
    guard let cgImage = image.cgImage(forProposedRect: &rect, context: nil, hints: nil),
          let context = CGContext(data: nil, width: 256, height: 256, bitsPerComponent: 8,
                                  bytesPerRow: 256 * 4, space: CGColorSpaceCreateDeviceRGB(),
                                  bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue),
          let data = context.data else {
        throw NSError(domain: "SwitchXIconCheck", code: 1,
                      userInfo: [NSLocalizedDescriptionKey: "Cannot decode icon pixels"])
    }
    context.draw(cgImage, in: CGRect(x: 0, y: 0, width: 256, height: 256))
    let pixels = data.bindMemory(to: UInt8.self, capacity: 256 * 256 * 4)
    var left = 256, right = -1
    for y in 0..<256 {
        for x in 0..<256 {
            let offset = (y * 256 + x) * 4
            let red = Int(pixels[offset]), green = Int(pixels[offset + 1]), blue = Int(pixels[offset + 2])
            // Follow the original artwork, excluding the navy fill and system-added grey plate.
            if pixels[offset + 3] > 128 && blue > 180 && blue - red > 75 && blue >= green {
                left = min(left, x)
                right = max(right, x)
            }
        }
    }
    guard right >= left else {
        throw NSError(domain: "SwitchXIconCheck", code: 2,
                      userInfo: [NSLocalizedDescriptionKey: "SwitchX artwork is missing"])
    }
    return Double(right - left + 1) / 256
}

guard (2...3).contains(CommandLine.arguments.count) else {
    fputs("Usage: swift scripts/check-macos-icon.swift ABS_APP_BUNDLE [SNAPSHOT_PNG]\n", stderr)
    exit(2)
}
let root = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent()
let bundle = URL(fileURLWithPath: CommandLine.arguments[1]).standardizedFileURL
guard FileManager.default.fileExists(atPath: bundle.appendingPathComponent("Contents/Info.plist").path),
      let source = NSImage(contentsOf: root.appendingPathComponent("assets/app-icon.png")) else {
    fputs("App bundle or source icon is missing\n", stderr)
    exit(2)
}
NSWorkspace.shared.noteFileSystemChanged(bundle.path)
let rendered = NSWorkspace.shared.icon(forFile: bundle.path)
if CommandLine.arguments.count == 3,
   let tiff = rendered.tiffRepresentation,
   let png = NSBitmapImageRep(data: tiff)?.representation(using: .png, properties: [:]) {
    try png.write(to: URL(fileURLWithPath: CommandLine.arguments[2]))
}
let sourceWidth = try artworkWidth(source)
let renderedWidth = try artworkWidth(rendered)
let scale = renderedWidth / sourceWidth
print(String(format: "Original artwork %.1f%%; native artwork %.1f%%; native/source scale %.3f",
             sourceWidth * 100, renderedWidth * 100, scale))
guard scale >= 0.78 else {
    fputs("FAIL: artwork is scaled down again inside a system-added plate\n", stderr)
    exit(1)
}
print("PASS: artwork uses the standard macOS icon scale")
