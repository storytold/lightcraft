//
//  PhotoPickerBridge.swift
//  LightCraft iPad
//
//  Clean-room Swift bridge for native iPad photo import (PHPicker)
//  and file/catalog import (UIDocumentPicker).
//

import UIKit
import PhotosUI
import UniformTypeIdentifiers
import Vision

public typealias PickerCompletion = @convention(c) (UnsafeMutableRawPointer?, UnsafePointer<CChar>?) -> Void

final class PhotoPickerManager: NSObject, PHPickerViewControllerDelegate, UIDocumentPickerDelegate {
    static let shared = PhotoPickerManager()

    private var activeCompletion: PickerCompletion?
    private var activeContext: UnsafeMutableRawPointer?

    private let importDirectory: URL = {
        let docs = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
        let dir = docs.appendingPathComponent("Imported", isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir
    }()

    private let exportDirectory: URL = {
        let docs = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
        let dir = docs.appendingPathComponent("Exports", isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir
    }()

    // MARK: - Entry Point

    func showPicker(
        kind: UInt32,
        title: String?,
        filterName: String?,
        filterExts: [String],
        fileName: String?,
        context: UnsafeMutableRawPointer?,
        completion: @escaping PickerCompletion
    ) {
        self.activeContext = context
        self.activeCompletion = completion

        let presentBlock = { [weak self] in
            guard let self = self else { return }

            guard let presenter = self.topViewController() else {
                NSLog("[PhotoPickerBridge] Error: No top view controller found")
                self.finish(with: [])
                return
            }

            switch kind {
            case 0: // PickKind::Files
                if filterExts.contains("lrcat") || filterExts.contains("lcpreset") {
                    self.presentDocumentPicker(forTypes: filterExts, multiple: true, from: presenter)
                } else {
                    self.presentPHPicker(multiple: true, from: presenter)
                }

            case 1: // PickKind::File
                if filterExts.contains("lrcat") {
                    self.presentDocumentPicker(forTypes: ["lrcat"], multiple: false, from: presenter)
                } else {
                    self.presentPHPicker(multiple: false, from: presenter)
                }

            case 2: // PickKind::Folder
                self.presentFolderPicker(from: presenter)

            case 3: // PickKind::Save
                let suggestedName = fileName ?? "Export"
                let saveUrl = self.exportDirectory.appendingPathComponent(suggestedName)
                self.finish(with: [saveUrl.path])

            default:
                self.finish(with: [])
            }
        }

        if Thread.isMainThread {
            presentBlock()
        } else {
            DispatchQueue.main.async(execute: presentBlock)
        }
    }

    // MARK: - PHPicker (Photo Library & ProRAW)

    private func presentPHPicker(multiple: Bool, from presenter: UIViewController) {
        var config = PHPickerConfiguration(photoLibrary: .shared())
        config.selectionLimit = multiple ? 0 : 1
        config.filter = .images
        config.preferredAssetRepresentationMode = .current

        let picker = PHPickerViewController(configuration: config)
        picker.delegate = self
        picker.modalPresentationStyle = .pageSheet
        presenter.present(picker, animated: true)
    }

    func picker(_ picker: PHPickerViewController, didFinishPicking results: [PHPickerResult]) {
        picker.dismiss(animated: true)

        guard !results.isNilOrEmpty else {
            finish(with: [])
            return
        }

        let group = DispatchGroup()
        var importedPaths: [String] = []
        let lock = NSLock()

        for (index, result) in results.enumerated() {
            group.enter()
            let provider = result.itemProvider

            if provider.hasItemConformingToTypeIdentifier(UTType.rawImage.identifier) {
                provider.loadFileRepresentation(forTypeIdentifier: UTType.rawImage.identifier) { [weak self] url, error in
                    defer { group.leave() }
                    guard let self = self, let url = url, error == nil else { return }
                    if let saved = self.copyToSandbox(from: url, prefix: "raw_\(index)") {
                        lock.lock()
                        importedPaths.append(saved)
                        lock.unlock()
                    }
                }
            } else if provider.hasItemConformingToTypeIdentifier(UTType.image.identifier) {
                provider.loadFileRepresentation(forTypeIdentifier: UTType.image.identifier) { [weak self] url, error in
                    defer { group.leave() }
                    guard let self = self, let url = url, error == nil else { return }
                    if let saved = self.copyToSandbox(from: url, prefix: "img_\(index)") {
                        lock.lock()
                        importedPaths.append(saved)
                        lock.unlock()
                    }
                }
            } else {
                group.leave()
            }
        }

        group.notify(queue: .main) { [weak self] in
            self?.finish(with: importedPaths)
        }
    }

    // MARK: - Document Picker (Files, SD Cards, SSDs, Lightroom Catalogs)

    private func presentDocumentPicker(forTypes exts: [String], multiple: Bool, from presenter: UIViewController) {
        var contentTypes: [UTType] = []
        for ext in exts {
            if let type = UTType(filenameExtension: ext) {
                contentTypes.append(type)
            }
        }
        if contentTypes.isEmpty {
            contentTypes = [.item]
        }

        let picker = UIDocumentPickerViewController(forOpeningContentTypes: contentTypes, asCopy: true)
        picker.delegate = self
        picker.allowsMultipleSelection = multiple
        picker.modalPresentationStyle = .pageSheet
        presenter.present(picker, animated: true)
    }

    private func presentFolderPicker(from presenter: UIViewController) {
        let picker = UIDocumentPickerViewController(forOpeningContentTypes: [.folder], asCopy: false)
        picker.delegate = self
        picker.allowsMultipleSelection = false
        picker.modalPresentationStyle = .pageSheet
        presenter.present(picker, animated: true)
    }

    private var activeSecurityScopedUrls: [URL] = []

    // MARK: - Remembered folders
    // Access to a folder outside the app (iCloud Drive, another app's files) lasts only for the session it was picked in.
    // A bookmark of each picked folder is kept, and access is restored at launch, so photos added in place from it
    // are still reachable after a restart and nothing has to be picked again.
    private let bookmarkKey = "lightcraft.folderBookmarks"

    private func rememberFolder(_ url: URL) {
        guard let data = try? url.bookmarkData(options: [], includingResourceValuesForKeys: nil, relativeTo: nil) else {
            NSLog("[PhotoPickerBridge] could not bookmark %@", url.path)
            return
        }
        var all = UserDefaults.standard.dictionary(forKey: bookmarkKey) as? [String: Data] ?? [:]
        all[url.path] = data
        UserDefaults.standard.set(all, forKey: bookmarkKey)
    }

    /// Call once at launch, before the library opens.
    func restoreRememberedFolders() {
        guard let all = UserDefaults.standard.dictionary(forKey: bookmarkKey) as? [String: Data] else { return }
        var kept = all
        for (oldPath, data) in all {
            var stale = false
            guard let url = try? URL(resolvingBookmarkData: data, options: [], relativeTo: nil, bookmarkDataIsStale: &stale) else {
                // an external drive that is not plugged in does not resolve: keep its bookmark for the next launch
                NSLog("[PhotoPickerBridge] a remembered folder is not reachable (drive unplugged?): %@", oldPath)
                continue
            }
            // restoring runs again each time the app comes back to the foreground (a drive may have been plugged in
            // meanwhile), so a folder already held open is not opened twice
            if !activeSecurityScopedUrls.contains(where: { $0.path == url.path }), url.startAccessingSecurityScopedResource() {
                activeSecurityScopedUrls.append(url)
            }
            NSLog("[PhotoPickerBridge] restored access to %@", url.path)
            if stale || url.path != oldPath {
                kept[oldPath] = nil
                if let fresh = try? url.bookmarkData(options: [], includingResourceValuesForKeys: nil, relativeTo: nil) {
                    kept[url.path] = fresh
                }
            }
        }
        UserDefaults.standard.set(kept, forKey: bookmarkKey)
    }

    func documentPicker(_ controller: UIDocumentPickerViewController, didPickDocumentsAt urls: [URL]) {
        var paths: [String] = []
        for url in urls {
            if url.hasDirectoryPath {
                if url.startAccessingSecurityScopedResource() {
                    activeSecurityScopedUrls.append(url)
                }
                rememberFolder(url)
                paths.append(url.path)
            } else {
                let shouldStop = url.startAccessingSecurityScopedResource()
                if let saved = copyToSandbox(from: url, prefix: "doc") {
                    paths.append(saved)
                }
                if shouldStop {
                    url.stopAccessingSecurityScopedResource()
                }
            }
        }
        finish(with: paths)
    }

    func documentPickerWasCancelled(_ controller: UIDocumentPickerViewController) {
        finish(with: [])
    }

    // MARK: - Helper

    private func copyToSandbox(from source: URL, prefix: String) -> String? {
        let ext = source.pathExtension
        let name = "\(prefix)_\(UUID().uuidString.prefix(8)).\(ext)"
        let destination = importDirectory.appendingPathComponent(name)

        do {
            if FileManager.default.fileExists(atPath: destination.path) {
                try FileManager.default.removeItem(at: destination)
            }
            try FileManager.default.copyItem(at: source, to: destination)
            return destination.path
        } catch {
            NSLog("[PhotoPickerBridge] Failed to copy item: %@", error.localizedDescription)
            return nil
        }
    }

    private func finish(with paths: [String]) {
        guard let completion = activeCompletion else { return }
        let context = activeContext
        activeCompletion = nil
        activeContext = nil

        let jsonData = (try? JSONSerialization.data(withJSONObject: paths, options: [])) ?? Data()
        let jsonString = String(data: jsonData, encoding: .utf8) ?? "[]"

        jsonString.withCString { cStr in
            completion(context, cStr)
        }
    }

    func topViewController() -> UIViewController? {
        let scenes = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
        guard let window = scenes.first?.windows.first(where: { $0.isKeyWindow }) ?? scenes.first?.windows.first ?? UIApplication.shared.windows.first else {
            return nil
        }
        var top = window.rootViewController
        if top == nil {
            let fallbackVC = UIViewController()
            window.rootViewController = fallbackVC
            top = fallbackVC
        }
        while let presented = top?.presentedViewController {
            top = presented
        }
        return top
    }
}

private extension Collection {
    var isNilOrEmpty: Bool {
        return isEmpty
    }
}

// MARK: - C FFI Exported Functions

@_cdecl("ios_show_native_picker")
public func ios_show_native_picker(
    kind: UInt32,
    title: UnsafePointer<CChar>?,
    filterName: UnsafePointer<CChar>?,
    filterExts: UnsafePointer<CChar>?,
    fileName: UnsafePointer<CChar>?,
    context: UnsafeMutableRawPointer?,
    completion: @escaping PickerCompletion
) {
    let titleStr = title.flatMap { String(cString: $0) }
    let filterNameStr = filterName.flatMap { String(cString: $0) }
    let filterExtsStr = filterExts.flatMap { String(cString: $0) } ?? ""
    let fileNameStr = fileName.flatMap { String(cString: $0) }

    let exts = filterExtsStr.split(separator: ",").map { String($0) }

    PhotoPickerManager.shared.showPicker(
        kind: kind,
        title: titleStr,
        filterName: filterNameStr,
        filterExts: exts,
        fileName: fileNameStr,
        context: context,
        completion: completion
    )
}

@_cdecl("ios_share_file")
public func ios_share_file(_ path_cstr: UnsafePointer<CChar>?) -> Bool {
    guard let path_cstr = path_cstr else { return false }
    let path = String(cString: path_cstr)
    let url = URL(fileURLWithPath: path)
    guard FileManager.default.fileExists(atPath: path) else { return false }

    DispatchQueue.main.async {
        guard let presenter = PhotoPickerManager.shared.topViewController() else { return }
        let activityVC = UIActivityViewController(activityItems: [url], applicationActivities: nil)
        if let popover = activityVC.popoverPresentationController {
            popover.sourceView = presenter.view
            popover.sourceRect = CGRect(x: presenter.view.bounds.midX, y: presenter.view.bounds.midY, width: 0, height: 0)
            popover.permittedArrowDirections = []
        }
        presenter.present(activityVC, animated: true)
    }
    return true
}

@_cdecl("ios_share_files")
public func ios_share_files(_ paths_json: UnsafePointer<CChar>?) -> Bool {
    guard let paths_json = paths_json else { return false }
    let jsonStr = String(cString: paths_json)
    guard let data = jsonStr.data(using: .utf8),
          let paths = try? JSONDecoder().decode([String].self, from: data) else {
        return false
    }
    let urls = paths.map { URL(fileURLWithPath: $0) }.filter { FileManager.default.fileExists(atPath: $0.path) }
    guard !urls.isEmpty else { return false }

    DispatchQueue.main.async {
        guard let presenter = PhotoPickerManager.shared.topViewController() else { return }
        let activityVC = UIActivityViewController(activityItems: urls, applicationActivities: nil)
        if let popover = activityVC.popoverPresentationController {
            popover.sourceView = presenter.view
            popover.sourceRect = CGRect(x: presenter.view.bounds.midX, y: presenter.view.bounds.midY, width: 0, height: 0)
            popover.permittedArrowDirections = []
        }
        presenter.present(activityVC, animated: true)
    }
    return true
}

@_cdecl("ios_open_url")
public func ios_open_url(_ cstr: UnsafePointer<CChar>?) -> Bool {
    guard let cstr = cstr else { return false }
    let str = String(cString: cstr)
    guard let url = URL(string: str), url.scheme == "https" else { return false }
    DispatchQueue.main.async {
        UIApplication.shared.open(url, options: [:], completionHandler: nil)
    }
    return true
}

@_cdecl("ios_log")
public func ios_log(_ cstr: UnsafePointer<CChar>?) {
    guard let cstr = cstr else { return }
    let str = String(cString: cstr)
    NSLog("%@", str)
}

// MARK: - Safe Area Insets FFI

private var cachedSafeArea: UIEdgeInsets = UIEdgeInsets(top: 24, left: 0, bottom: 20, right: 0)

/// Keeps the last real insets. A window that is attached to a scene and on screen may report zeros for real (a Stage
/// Manager window, a Split View pane away from the screen edge), and those replace the cache; zeros from a window that is
/// not attached yet (launch) do not.
private func refreshSafeArea(from window: UIWindow?) {
    guard let window = window, let insets = lcRealSafeAreaInsets(of: window) else { return }
    let attached = window.windowScene != nil && window.bounds.width > 0 && window.bounds.height > 0
    if insets.top > 0 || insets.bottom > 0 || attached {
        cachedSafeArea = insets
    }
}

@_cdecl("ios_update_safe_area")
public func ios_update_safe_area() {
    let block = {
        let scenes = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
        let window = scenes.first?.windows.first(where: { $0.isKeyWindow }) ?? scenes.first?.windows.first
        refreshSafeArea(from: window)
    }
    if Thread.isMainThread {
        block()
    } else {
        DispatchQueue.main.async(execute: block)
    }
}

@_cdecl("ios_get_safe_area")
public func ios_get_safe_area(
    top: UnsafeMutablePointer<Float>?,
    bottom: UnsafeMutablePointer<Float>?,
    left: UnsafeMutablePointer<Float>?,
    right: UnsafeMutablePointer<Float>?
) {
    if Thread.isMainThread {
        let scenes = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
        let window = scenes.first?.windows.first(where: { $0.isKeyWindow }) ?? scenes.first?.windows.first
        refreshSafeArea(from: window)
    }
    top?.pointee = Float(cachedSafeArea.top)
    bottom?.pointee = Float(cachedSafeArea.bottom)
    left?.pointee = Float(cachedSafeArea.left)
    right?.pointee = Float(cachedSafeArea.right)
}


@_cdecl("ios_toggle_keyboard")
public func ios_toggle_keyboard(_ show: Bool) {
    DispatchQueue.main.async {
        let scenes = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
        guard let window = scenes.first?.windows.first(where: { $0.isKeyWindow }) ?? scenes.first?.windows.first ?? UIApplication.shared.windows.first else {
            return
        }
        window.overrideUserInterfaceStyle = .dark
        window.rootViewController?.overrideUserInterfaceStyle = .dark
        if show {
            if let root = window.rootViewController?.view {
                if let tf = root as? UITextField {
                    tf.keyboardAppearance = .dark
                }
                _ = root.becomeFirstResponder()
            }
        } else {
            window.endEditing(true)
        }
    }
}


@_cdecl("ios_get_system_theme")
public func ios_get_system_theme() -> Int32 {
    let scenes = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
    let style = scenes.first?.traitCollection.userInterfaceStyle ?? UITraitCollection.current.userInterfaceStyle
    switch style {
    case .light:
        return 1
    case .dark:
        return 2
    default:
        return 0
    }
}


// MARK: - Apple Vision masks (no model download)

@_silgen_name("lightcraft_ios_set_segmenter")
private func lightcraft_ios_set_segmenter(
    _ cb: @convention(c) (UnsafePointer<UInt8>?, Int32, Int32, UInt64, UnsafePointer<Float>?, Int32, UnsafeMutablePointer<Float>?, Int32) -> Int32
)

/// Subject / object selection with `VNGenerateForegroundInstanceMaskRequest` (iPadOS 17+), on the
/// Neural Engine. The instance map of the last photo is kept, so each tap only picks instances.
@available(iOS 17.0, *)
private final class VisionSegmenter {
    private var key: UInt64 = .max
    private var handler: VNImageRequestHandler?
    private var observation: VNInstanceMaskObservation?
    private let lock = NSLock()

    func segment(rgb: UnsafePointer<UInt8>, w: Int, h: Int, key: UInt64, clicks: [(Float, Float, Bool)],
                 out: UnsafeMutablePointer<Float>, side: Int) -> Int32 {
        lock.lock(); defer { lock.unlock() }
        if key != self.key || handler == nil {
            guard let cg = Self.image(rgb: rgb, w: w, h: h) else { return -3 }
            let request = VNGenerateForegroundInstanceMaskRequest()
            let newHandler = VNImageRequestHandler(cgImage: cg, options: [:])
            do { try newHandler.perform([request]) } catch { return -4 }
            self.key = key
            handler = newHandler
            observation = request.results?.first
        }
        guard let observation, let handler else { return 0 }
        var instances = IndexSet()
        let include = clicks.filter { $0.2 }
        if include.isEmpty {
            instances = observation.allInstances
        } else {
            for c in include { if let i = instance(at: c, in: observation), i > 0 { instances.insert(i) } }
            for c in clicks where !c.2 { if let i = instance(at: c, in: observation) { instances.remove(i) } }
        }
        if instances.isEmpty { return 0 }
        guard let mask = try? observation.generateScaledMaskForImage(forInstances: instances, from: handler) else { return -5 }
        Self.sample(mask, into: out, side: side)
        return 1
    }

    /// The instance under a click, or the nearest one within a fingertip so taps near an edge still land.
    private func instance(at c: (Float, Float, Bool), in o: VNInstanceMaskObservation) -> Int? {
        let map = o.instanceMask
        CVPixelBufferLockBaseAddress(map, .readOnly)
        defer { CVPixelBufferUnlockBaseAddress(map, .readOnly) }
        guard let base = CVPixelBufferGetBaseAddress(map) else { return nil }
        let mw = CVPixelBufferGetWidth(map), mh = CVPixelBufferGetHeight(map), rowBytes = CVPixelBufferGetBytesPerRow(map)
        let cx = Int(max(0, min(1, c.0)) * Float(mw - 1)), cy = Int(max(0, min(1, c.1)) * Float(mh - 1))
        let reach = max(2, max(mw, mh) / 22)
        var best: (Int, Int)? = nil  // (distance², instance)
        for dy in stride(from: -reach, through: reach, by: 2) {
            for dx in stride(from: -reach, through: reach, by: 2) {
                let x = cx + dx, y = cy + dy
                if x < 0 || y < 0 || x >= mw || y >= mh { continue }
                let v = Int(base.load(fromByteOffset: y * rowBytes + x, as: UInt8.self))
                if v == 0 { continue }
                let d = dx * dx + dy * dy
                if best == nil || d < best!.0 { best = (d, v) }
            }
        }
        return best?.1
    }

    private static func image(rgb: UnsafePointer<UInt8>, w: Int, h: Int) -> CGImage? {
        var rgba = [UInt8](repeating: 255, count: w * h * 4)
        for i in 0..<(w * h) {
            rgba[i * 4] = rgb[i * 3]; rgba[i * 4 + 1] = rgb[i * 3 + 1]; rgba[i * 4 + 2] = rgb[i * 3 + 2]
        }
        guard let provider = CGDataProvider(data: Data(rgba) as CFData), let space = CGColorSpace(name: CGColorSpace.sRGB) else { return nil }
        return CGImage(width: w, height: h, bitsPerComponent: 8, bitsPerPixel: 32, bytesPerRow: w * 4, space: space,
                       bitmapInfo: CGBitmapInfo(rawValue: CGImageAlphaInfo.noneSkipLast.rawValue), provider: provider,
                       decode: nil, shouldInterpolate: false, intent: .defaultIntent)
    }

    /// Coverage 0...1 (one float per pixel) → `side × side` logits (±8) for the engine's soft mask.
    private static func sample(_ buf: CVPixelBuffer, into out: UnsafeMutablePointer<Float>, side: Int) {
        CVPixelBufferLockBaseAddress(buf, .readOnly)
        defer { CVPixelBufferUnlockBaseAddress(buf, .readOnly) }
        guard let base = CVPixelBufferGetBaseAddress(buf) else { return }
        let bw = CVPixelBufferGetWidth(buf), bh = CVPixelBufferGetHeight(buf), rowBytes = CVPixelBufferGetBytesPerRow(buf)
        for sy in 0..<side {
            for sx in 0..<side {
                var sum: Float = 0
                for (ox, oy) in [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)] as [(Double, Double)] {
                    let x = min(bw - 1, Int((Double(sx) + ox) / Double(side) * Double(bw)))
                    let y = min(bh - 1, Int((Double(sy) + oy) / Double(side) * Double(bh)))
                    sum += base.load(fromByteOffset: y * rowBytes + x * 4, as: Float.self)
                }
                out[sy * side + sx] = (sum / 4 - 0.5) * 16
            }
        }
    }
}

@available(iOS 17.0, *)
private let visionSegmenter = VisionSegmenter()

private func visionSegmentEntry(
    _ rgb: UnsafePointer<UInt8>?, _ w: Int32, _ h: Int32, _ key: UInt64,
    _ clicks: UnsafePointer<Float>?, _ n: Int32, _ out: UnsafeMutablePointer<Float>?, _ side: Int32
) -> Int32 {
    guard #available(iOS 17.0, *) else { return -2 }
    guard let rgb, let out, w > 0, h > 0, side > 0 else { return -1 }
    var list: [(Float, Float, Bool)] = []
    if let clicks { for i in 0..<Int(max(0, n)) { list.append((clicks[i * 3], clicks[i * 3 + 1], clicks[i * 3 + 2] > 0.5)) } }
    return visionSegmenter.segment(rgb: rgb, w: Int(w), h: Int(h), key: key, clicks: list, out: out, side: Int(side))
}

/// Hand LightCraft the Vision segmenter (call once before `lightcraft_start`).
func installVisionSegmenter() {
    lightcraft_ios_set_segmenter(visionSegmentEntry)
}

// MARK: - Apple Pencil digit strokes

@_silgen_name("lightcraft_ios_pencil_stroke")
private func lightcraft_ios_pencil_stroke(_ xs: UnsafePointer<Float>, _ ys: UnsafePointer<Float>, _ n: Int32)

/// Watches Pencil strokes only (never consumes them) and reports each finished stroke, so drawing
/// 1-5 on the photo rates it. Fingers are not allowed, so touch gestures are untouched.
final class PencilStrokeRecognizer: UIGestureRecognizer {
    private var xs: [Float] = []
    private var ys: [Float] = []

    override init(target: Any?, action: Selector?) {
        super.init(target: target, action: action)
        allowedTouchTypes = [NSNumber(value: UITouch.TouchType.pencil.rawValue)]
        cancelsTouchesInView = false
        delaysTouchesBegan = false
        delaysTouchesEnded = false
    }

    private func add(_ touch: UITouch, _ event: UIEvent?) {
        guard let view = self.view else { return }
        for t in event?.coalescedTouches(for: touch) ?? [touch] where xs.count < 4096 {
            let p = t.location(in: view)
            xs.append(Float(p.x)); ys.append(Float(p.y))
        }
    }

    override func touchesBegan(_ touches: Set<UITouch>, with event: UIEvent) {
        xs.removeAll(); ys.removeAll()
        if let t = touches.first { add(t, event) }
    }

    override func touchesMoved(_ touches: Set<UITouch>, with event: UIEvent) {
        if let t = touches.first { add(t, event) }
    }

    override func touchesEnded(_ touches: Set<UITouch>, with event: UIEvent) {
        if let t = touches.first { add(t, event) }
        if xs.count >= 2 { lightcraft_ios_pencil_stroke(xs, ys, Int32(xs.count)) }
        xs.removeAll(); ys.removeAll()
        state = .failed  // never "recognized": the stroke still reaches the app
    }

    override func touchesCancelled(_ touches: Set<UITouch>, with event: UIEvent) {
        xs.removeAll(); ys.removeAll()
        state = .failed
    }
}
