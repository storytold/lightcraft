#!/bin/sh
# Build lightcraft-ios for the iOS Simulator, bundle it as an .app and run it on a booted
# simulator (or the one named by $1). Needs Xcode and `rustup target add aarch64-apple-ios-sim`.
set -eu
cd "$(dirname "$0")/../../.."
CARGO=${CARGO:-cargo}
TARGET=aarch64-apple-ios-sim
PROFILE=${PROFILE:-debug}
[ "$PROFILE" = release ] && FLAG=--release || FLAG=
$CARGO build $FLAG -p lightcraft-mobile --bin lightcraft-ios --target $TARGET
OUT=${CARGO_TARGET_DIR:-target}/$TARGET/$PROFILE
APP=$OUT/LightCraft.app
rm -rf "$APP" && mkdir -p "$APP"
cp "$OUT/lightcraft-ios" "$APP/"
cp apps/lightcraft-mobile/ios/Info.plist "$APP/"
# the app icon (assets/app-icon/ios, rendered by packaging/icons.sh): actool writes Assets.car and
# the icon PNGs into the bundle, and the CFBundleIcons keys merged into Info.plist
xcrun actool assets/app-icon/ios/Assets.xcassets --compile "$APP" --platform iphonesimulator \
    --minimum-deployment-target 17.0 --target-device iphone --target-device ipad --app-icon AppIcon \
    --output-partial-info-plist "$OUT/AppIcon.plist" --output-format human-readable-text --notices --warnings >/dev/null
/usr/libexec/PlistBuddy -c "Merge $OUT/AppIcon.plist" "$APP/Info.plist" >/dev/null
codesign -f -s - "$APP"
DEVICE=${1:-booted}
xcrun simctl install "$DEVICE" "$APP"
exec xcrun simctl launch --console-pty --terminate-running-process "$DEVICE" ai.storyteller.lightcraft.mobile
