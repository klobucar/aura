import CoreGraphics
import ImageIO
import UniformTypeIdentifiers
import XCTest
@testable import Aura

final class AvatarImageTests: XCTestCase {
    private let pngMagic: [UInt8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]
    private let jpegMagic: [UInt8] = [0xFF, 0xD8, 0xFF]

    /// A gradient (or noisy) test image, optionally with a transparent corner.
    private func makeImage(width: Int, height: Int, transparent: Bool = false, noisy: Bool = false) -> Data {
        let ctx = CGContext(
            data: nil, width: width, height: height, bitsPerComponent: 8, bytesPerRow: 0,
            space: CGColorSpace(name: CGColorSpace.sRGB)!,
            bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
        if noisy {
            var rng = SystemRandomNumberGenerator()
            for _ in 0..<4000 {
                ctx.setFillColor(CGColor(
                    red: .random(in: 0...1, using: &rng), green: .random(in: 0...1, using: &rng),
                    blue: .random(in: 0...1, using: &rng), alpha: 1))
                ctx.fill(CGRect(
                    x: Int.random(in: 0..<width), y: Int.random(in: 0..<height),
                    width: Int.random(in: 1...40), height: Int.random(in: 1...40)))
            }
        } else {
            ctx.setFillColor(CGColor(red: 0.2, green: 0.5, blue: 0.9, alpha: 1))
            ctx.fill(CGRect(x: 0, y: 0, width: width, height: height))
        }
        if transparent {
            ctx.clear(CGRect(x: 0, y: 0, width: width / 2, height: height / 2))
        }
        let out = NSMutableData()
        let dest = CGImageDestinationCreateWithData(out, UTType.png.identifier as CFString, 1, nil)!
        CGImageDestinationAddImage(dest, ctx.makeImage()!, nil)
        CGImageDestinationFinalize(dest)
        return out as Data
    }

    private func size(of data: Data) -> (Int, Int)? {
        guard let src = CGImageSourceCreateWithData(data as CFData, nil),
              let img = CGImageSourceCreateImageAtIndex(src, 0, nil) else { return nil }
        return (img.width, img.height)
    }

    func testOpaqueImageBecomesSquareJPEGWithinCap() throws {
        let out = try XCTUnwrap(AvatarImage.encode(makeImage(width: 1200, height: 900)))
        XCTAssertEqual(Array(out.prefix(3)), jpegMagic)
        XCTAssertLessThanOrEqual(out.count, AvatarImage.maxBytes)
        let dims = try XCTUnwrap(size(of: out))
        XCTAssertEqual(dims.0, 256)
        XCTAssertEqual(dims.1, 256)
    }

    func testTransparentImageStaysPNG() throws {
        let out = try XCTUnwrap(AvatarImage.encode(makeImage(width: 400, height: 400, transparent: true)))
        XCTAssertEqual(Array(out.prefix(8)), pngMagic)
        XCTAssertLessThanOrEqual(out.count, AvatarImage.maxBytes)
    }

    func testOddAspectRatiosAreCroppedToSquare() throws {
        for (w, h) in [(2000, 100), (100, 2000), (257, 513)] {
            let out = try XCTUnwrap(AvatarImage.encode(makeImage(width: w, height: h)), "\(w)x\(h)")
            let dims = try XCTUnwrap(size(of: out))
            XCTAssertEqual(dims.0, dims.1, "\(w)x\(h)")
            XCTAssertLessThanOrEqual(dims.0, 256)
        }
    }

    func testSmallImagesAreNotUpscaled() throws {
        let out = try XCTUnwrap(AvatarImage.encode(makeImage(width: 64, height: 100)))
        XCTAssertEqual(size(of: out)?.0, 64)
    }

    func testNoisyImageStillFitsTheCap() throws {
        let out = try XCTUnwrap(AvatarImage.encode(makeImage(width: 1024, height: 1024, noisy: true)))
        XCTAssertLessThanOrEqual(out.count, AvatarImage.maxBytes)
    }

    func testTinyCapShrinksOrFails() throws {
        let data = makeImage(width: 1024, height: 1024, noisy: true)
        if let out = AvatarImage.encode(data, maxBytes: 4096) {
            XCTAssertLessThanOrEqual(out.count, 4096)
        }
    }

    func testNonImageDataIsRejected() {
        XCTAssertNil(AvatarImage.encode(Data("not an image".utf8)))
        XCTAssertNil(AvatarImage.encode(Data()))
    }
}
