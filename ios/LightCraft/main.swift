import Foundation

@_silgen_name("lightcraft_start")
private func lightcraft_start(_ libraryPath: UnsafePointer<CChar>) -> Int32

NSLog("[LightCraft-Swift] Application entry in main.swift")
let documents = URL(fileURLWithPath: NSHomeDirectory(), isDirectory: true).appendingPathComponent("Documents", isDirectory: true)
let library = documents.appendingPathComponent("LightCraft Library", isDirectory: true)
try? FileManager.default.createDirectory(at: library, withIntermediateDirectories: true)

installVisionSegmenter()
PhotoPickerManager.shared.restoreRememberedFolders()
let result = library.path.withCString { lightcraft_start($0) }
NSLog("[LightCraft-Swift] lightcraft_start returned with result: %d", result)
if result != 0 {
    NSLog("LightCraft iPad exited with status %d", result)
}
