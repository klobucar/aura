import CoreGraphics
import Foundation
import ImageIO
import UniformTypeIdentifiers

/// Turns whatever image the user picked into something the server accepts as
/// an avatar: a square of at most 256 px, as JPEG or (for images that really
/// use transparency) PNG, never over 128 KiB.
nonisolated enum AvatarImage {
    /// The server's limit on `avatar_data`.
    static let maxBytes = 128 * 1024
    /// Edge length of the square avatar.
    static let side = 256

    /// Center-crops `data` to a square, scales it down to at most `side`
    /// pixels, and encodes it. Returns nil if `data` isn't a decodable image
    /// or can't be brought under `maxBytes`.
    static func encode(_ data: Data, side: Int = AvatarImage.side, maxBytes: Int = AvatarImage.maxBytes) -> Data? {
        guard let source = CGImageSourceCreateWithData(data as CFData, nil) else { return nil }
        // Decode with the EXIF orientation applied; cap the decoded size so a
        // huge photo doesn't cost hundreds of MB.
        let options: [CFString: Any] = [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceCreateThumbnailWithTransform: true,
            kCGImageSourceThumbnailMaxPixelSize: 2048,
        ]
        guard let full = CGImageSourceCreateThumbnailAtIndex(source, 0, options as CFDictionary),
              full.width > 0, full.height > 0
        else { return nil }

        let edge = min(full.width, full.height)
        let crop = CGRect(
            x: (full.width - edge) / 2, y: (full.height - edge) / 2,
            width: edge, height: edge)
        guard let square = full.cropping(to: crop) else { return nil }

        // Never upscale; shrink further if a size doesn't fit the cap.
        var target = min(side, edge)
        while target >= 32 {
            if let pixels = render(square, side: target) {
                if hasTransparency(pixels), let png = encode(pixels, type: .png, quality: nil), png.count <= maxBytes {
                    return png
                }
                var quality = 0.85
                while quality >= 0.3 {
                    if let flat = flatten(pixels, side: target),
                       let jpeg = encode(flat, type: .jpeg, quality: quality), jpeg.count <= maxBytes {
                        return jpeg
                    }
                    quality -= 0.1
                }
            }
            target = target * 3 / 4
        }
        return nil
    }

    private static let colorSpace = CGColorSpace(name: CGColorSpace.sRGB)!
    private static let bitmapInfo = CGImageAlphaInfo.premultipliedLast.rawValue

    private static func render(_ image: CGImage, side: Int) -> CGImage? {
        guard let ctx = CGContext(
            data: nil, width: side, height: side, bitsPerComponent: 8, bytesPerRow: 0,
            space: colorSpace, bitmapInfo: bitmapInfo)
        else { return nil }
        ctx.interpolationQuality = .high
        ctx.draw(image, in: CGRect(x: 0, y: 0, width: side, height: side))
        return ctx.makeImage()
    }

    /// Composite over white; JPEG has no alpha.
    private static func flatten(_ image: CGImage, side: Int) -> CGImage? {
        guard let ctx = CGContext(
            data: nil, width: side, height: side, bitsPerComponent: 8, bytesPerRow: 0,
            space: colorSpace, bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue)
        else { return nil }
        ctx.setFillColor(CGColor(red: 1, green: 1, blue: 1, alpha: 1))
        ctx.fill(CGRect(x: 0, y: 0, width: side, height: side))
        ctx.draw(image, in: CGRect(x: 0, y: 0, width: side, height: side))
        return ctx.makeImage()
    }

    private static func hasTransparency(_ image: CGImage) -> Bool {
        guard let data = image.dataProvider?.data, let bytes = CFDataGetBytePtr(data) else { return false }
        // Rows can be padded, so walk them by stride.
        for y in 0..<image.height {
            let row = y * image.bytesPerRow
            for x in 0..<image.width where bytes[row + x * 4 + 3] < 255 { return true }
        }
        return false
    }

    private static func encode(_ image: CGImage, type: UTType, quality: Double?) -> Data? {
        let out = NSMutableData()
        guard let dest = CGImageDestinationCreateWithData(out, type.identifier as CFString, 1, nil) else { return nil }
        var props: [CFString: Any] = [:]
        if let quality { props[kCGImageDestinationLossyCompressionQuality] = quality }
        CGImageDestinationAddImage(dest, image, props as CFDictionary)
        return CGImageDestinationFinalize(dest) ? out as Data : nil
    }
}
