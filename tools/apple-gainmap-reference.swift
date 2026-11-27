// Optional macOS developer probe; no Apple framework is linked into LightCraft.
// swiftc tools/apple-gainmap-reference.swift -o /tmp/apple-gainmap-reference
// /tmp/apple-gainmap-reference INPUT OUTPUT_DIRECTORY
import CoreImage
import Foundation
import ImageIO

struct ProbeError: Error, CustomStringConvertible {
    let description: String
    init(_ description: String) { self.description = description }
}

func run() throws {
    let args = CommandLine.arguments
    guard args.count == 3 else { throw ProbeError("usage: apple-gainmap-reference INPUT OUTPUT_DIRECTORY") }
    let input = URL(fileURLWithPath: args[1])
    let output = URL(fileURLWithPath: args[2], isDirectory: true)
    // Refuse existing destinations: reference runs must not overwrite another run.
    guard !FileManager.default.fileExists(atPath: output.path) else { throw ProbeError("output directory already exists") }
    guard let source = CGImageSourceCreateWithURL(input as CFURL, nil),
          let properties = CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [String: Any],
          let auxiliary = CGImageSourceCopyAuxiliaryDataInfoAtIndex(source, 0, kCGImageAuxiliaryDataTypeHDRGainMap) as? [String: Any],
          let description = auxiliary[kCGImageAuxiliaryDataInfoDataDescription as String] as? [String: Any],
          let bytes = auxiliary[kCGImageAuxiliaryDataInfoData as String] as? Data,
          let mapWidth = description["Width"] as? Int,
          let mapHeight = description["Height"] as? Int,
          let rowBytes = description["BytesPerRow"] as? Int,
          let pixelFormat = description["PixelFormat"] as? UInt32,
          pixelFormat == 0x4c303038, // kCVPixelFormatType_OneComponent8
          mapWidth > 0, mapHeight > 0, mapWidth <= 32768, mapHeight <= 32768,
          mapWidth * mapHeight <= 32_000_000,
          rowBytes >= mapWidth, rowBytes <= 131072,
          bytes.count >= rowBytes * mapHeight else { throw ProbeError("no supported 8-bit Apple auxiliary map") }
    if CGImageSourceCopyAuxiliaryDataInfoAtIndex(source, 0, kCGImageAuxiliaryDataTypeISOGainMap) != nil {
        throw ProbeError("ISO gain maps are outside this legacy Apple probe")
    }
    guard let linear = CGColorSpace(name: CGColorSpace.extendedLinearDisplayP3),
          let sdr = CIImage(contentsOf: input, options: [.expandToHDR: false, .applyOrientationProperty: false]),
          let hdr = CIImage(contentsOf: input, options: [.expandToHDR: true, .applyOrientationProperty: false]) else { throw ProbeError("Core Image cannot decode the input") }
    let bounds = sdr.extent.integral
    guard bounds == hdr.extent.integral, bounds.origin == .zero,
          bounds.width > 0, bounds.height > 0, bounds.width <= 32768, bounds.height <= 32768,
          bounds.width * bounds.height <= 32_000_000 else { throw ProbeError("unsupported image extent (limit 32 MP)") }
    let width = Int(bounds.width), height = Int(bounds.height)
    let context = CIContext(options: [.workingFormat: CIFormat.RGBAf, .workingColorSpace: linear, .cacheIntermediates: false])
    // All files retain stored orientation. Core Image's bitmap row order is kept for both renders.
    func render(_ image: CIImage) -> Data {
        var rgba = [Float](repeating: 0, count: width * height * 4)
        rgba.withUnsafeMutableBytes { buffer in
            if let address = buffer.baseAddress {
                context.render(image, toBitmap: address, rowBytes: width * 16, bounds: bounds, format: .RGBAf, colorSpace: linear)
            }
        }
        var rgb = Data(capacity: width * height * 12)
        for index in 0..<(width * height) {
            for channel in 0..<3 {
                var bits = rgba[index * 4 + channel].bitPattern.littleEndian
                withUnsafeBytes(of: &bits) { rgb.append(contentsOf: $0) }
            }
        }
        return rgb
    }
    let maker = properties[kCGImagePropertyMakerAppleDictionary as String] as? [String: Any] ?? [:]
    var info: [String: Any] = ["width": width, "height": height, "map_width": mapWidth, "map_height": mapHeight,
        "macos": ProcessInfo.processInfo.operatingSystemVersionString,
        "imageio": Bundle(identifier: "com.apple.ImageIO")?.infoDictionary?["CFBundleShortVersionString"] as? String ?? "unknown",
        "coreimage_headroom": hdr.contentHeadroom, "orientation_applied": false,
        "space": "extended-linear-display-p3", "sample_format": "rgb-f32-little-endian"]
    if let value = maker["33"] as? NSNumber { info["maker33"] = value.doubleValue }
    if let value = maker["48"] as? NSNumber { info["maker48"] = value.doubleValue }
    if let metadata = auxiliary[kCGImageAuxiliaryDataInfoMetadata as String] {
        guard CFGetTypeID(metadata as CFTypeRef) == CGImageMetadataGetTypeID() else { throw ProbeError("invalid auxiliary metadata type") }
        // The Core Foundation type check above makes this bridge infallible.
        let tags = CGImageMetadataCopyTags(metadata as! CGImageMetadata) as? [CGImageMetadataTag] ?? []
        for tag in tags {
            if (CGImageMetadataTagCopyName(tag) as String?) == "HDRGainMapHeadroom",
               let number = CGImageMetadataTagCopyValue(tag) as? NSNumber {
                info["headroom"] = number.doubleValue
            } else if (CGImageMetadataTagCopyName(tag) as String?) == "HDRGainMapHeadroom",
                      let string = CGImageMetadataTagCopyValue(tag) as? String, let number = Double(string) {
                info["headroom"] = number
            }
        }
    }
    var map = Data(capacity: mapWidth * mapHeight)
    for row in 0..<mapHeight { map.append(bytes.subdata(in: (row * rowBytes)..<(row * rowBytes + mapWidth))) }
    let base = render(sdr), reference = render(hdr)
    let manifest = try JSONSerialization.data(withJSONObject: info, options: [.prettyPrinted, .sortedKeys])
    try FileManager.default.createDirectory(at: output, withIntermediateDirectories: true)
    try base.write(to: output.appendingPathComponent("base.rgbf32le"), options: .withoutOverwriting)
    try reference.write(to: output.appendingPathComponent("hdr.rgbf32le"), options: .withoutOverwriting)
    try map.write(to: output.appendingPathComponent("gain.gray8"), options: .withoutOverwriting)
    try manifest.write(to: output.appendingPathComponent("reference.json"), options: .withoutOverwriting)
    print(String(decoding: manifest, as: UTF8.self))
}

do { try run() } catch {
    FileHandle.standardError.write(Data("apple-gainmap-reference: \(error)\n".utf8))
    exit(1)
}
