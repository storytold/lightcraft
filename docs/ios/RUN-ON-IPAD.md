# Running LightCraft on iPad

LightCraft includes a native iPadOS application (`LightCraft iPad (fork)`) built on Rust and Swift with direct Metal GPU compute.

---

## 1. Prerequisites

- **Mac**: macOS Sonoma / Sequoia with Xcode 15+ or 16+ installed.
- **iPad**: Physical iPad running iPadOS 16.0 or newer (iPad Air M2/M4, iPad Pro M1/M2/M4, or iPad 10th gen) or iPad Simulator.
- **Rust Toolchain**:
  ```sh
  rustup target add aarch64-apple-ios
  rustup target add aarch64-apple-ios-sim
  ```
- **XcodeGen** (optional, for regenerating Xcode project):
  ```sh
  brew install xcodegen
  ```

---

## 2. Running on a Physical iPad (via USB-C)

### Step 1: Connect & Trust iPad
1. Connect your iPad to your Mac using a USB-C or Lightning cable.
2. If prompted on the iPad, tap **Trust This Computer** and enter your passcode.
3. On the iPad, enable **Developer Mode**:
   - Go to **Settings → Privacy & Security → Developer Mode**.
   - Toggle **Developer Mode** on and restart the device when prompted.

### Step 2: Configure Code Signing in Xcode
1. Open `ios/LightCraftiPad.xcodeproj` in Xcode:
   ```sh
   open ios/LightCraftiPad.xcodeproj
   ```
2. Select the **LightCraftiPad** project in the navigator, click on the **LightCraftiPad** target, and open **Signing & Capabilities**.
3. Under **Team**, select your Apple ID (Personal Team or Apple Developer Account).
4. Xcode will automatically generate a provisioning profile and signing certificate for bundle identifier `com.<your-name>.lightcraftpad`.

### Step 3: Build & Run
1. In Xcode's destination dropdown (top toolbar), select your connected physical iPad.
2. Press **⌘R** (or click the Play button) to build and run.
3. The Xcode build phase will invoke `cargo build --manifest-path ios/lightcraft-ios/Cargo.toml --target aarch64-apple-ios --release` and bundle the static library into the iOS app.
4. On the iPad, the app icon will appear. If running on a free Apple ID for the first time, go to **Settings → General → VPN & Device Management** on the iPad and tap **Trust <Your Apple ID>**.
5. Tap LightCraft iPad to launch!

---

## 3. Running on the iOS Simulator

1. To compile the Rust static library for simulator:
   ```sh
   export PATH="$HOME/.cargo/bin:$PATH"
   cargo build --manifest-path ios/lightcraft-ios/Cargo.toml --target aarch64-apple-ios-sim --release
   mkdir -p ios/build
   cp ios/target/aarch64-apple-ios-sim/release/liblightcraft_ios.a ios/build/liblightcraft_ios.a
   ```
2. Build and launch with `xcodebuild` and `simctl`:
   ```sh
   DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer xcrun xcodebuild \
     -project ios/LightCraftiPad.xcodeproj \
     -scheme LightCraftiPad \
     -destination 'platform=iOS Simulator,name=iPad Pro 13-inch (M5)' \
     build CODE_SIGNING_ALLOWED=NO CODE_SIGNING_REQUIRED=NO

   DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer xcrun simctl install \
     "iPad Pro 13-inch (M5)" \
     "/Users/kislayranjanneetandon/Library/Developer/Xcode/DerivedData/LightCraftiPad-*/Build/Products/Debug-iphonesimulator/LightCraft iPad (fork).app"

   DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer xcrun simctl launch \
     "iPad Pro 13-inch (M5)" com.kislay.lightcraftpad
   ```

---

## 4. Native iPad Features & Workflow

### Importing Photos
- **Photo Library & RAW/ProRAW**: Tap **Import → Photos** to open `PHPickerViewController`. Supports multi-selection of camera RAWs (DNG, ARW, CR3, NEF, RAF, RW2) and ProRAW files.
- **External Storage & Files**: Tap **Import → Files** or connect an external USB-C SSD or SD card reader. Opens `UIDocumentPickerViewController` with security-scoped sandboxed access.
- **Lightroom Classic Catalogs**: Tap **Import Lightroom Catalog** to select `.lrcat` catalogs and import photo hierarchies and metadata.

### Editing & Culling
- **Touch UI**: Enabled by default with touch-friendly 44pt controls.
- **Gestures**:
  - **Pinch to Zoom**: Seamless zoom into 100%–800% detail view.
  - **Two-Finger Drag**: Smooth panning across the image.
  - **Double Tap**: Instantly toggle between Fit view and 100% 1:1 view.
- **Scene-Adaptive AI Auto Tone & Auto WB**: Single-tap or batch auto adjustments with backlit subject compensation.

### Exporting & Sharing
- **Files App Access**: Exported photos are saved into the app's sandboxed `Documents/Exports/` directory, immediately visible in the iPad **Files** app under **On My iPad → LightCraft iPad**.
- **System Share Sheet**: Use the native iOS Share Sheet (`UIActivityViewController`) to AirDrop full-resolution photos, save to Photos camera roll, or send via Mail and Messages.
