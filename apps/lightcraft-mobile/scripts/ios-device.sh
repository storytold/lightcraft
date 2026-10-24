#!/bin/sh
# Build lightcraft-ios for iPhones and iPads, bundle it as a signed .app (with the app icon) and
# install it on each connected device named by UDID (`xcrun devicectl list devices`). Needs Xcode,
# `rustup target add aarch64-apple-ios`, an Apple Development certificate and a development
# provisioning profile that lists the devices:
#
#   IOS_SIGN_IDENTITY  the certificate's name or SHA-1 (`security find-identity -v -p codesigning`)
#   IOS_PROFILE        the .mobileprovision file
#   IOS_TEAM_ID        the team id (the profile's TeamIdentifier)
#   IOS_BUNDLE_ID      optional, default ai.storyteller.lightcraft.mobile (the profile must allow it)
#
# PROFILE=debug for a debug build (release by default: debug is slow on a phone).
set -eu
cd "$(dirname "$0")/../../.."
: "${IOS_SIGN_IDENTITY:?set IOS_SIGN_IDENTITY}" "${IOS_PROFILE:?set IOS_PROFILE}" "${IOS_TEAM_ID:?set IOS_TEAM_ID}"
BUNDLE_ID=${IOS_BUNDLE_ID:-ai.storyteller.lightcraft.mobile}
[ $# -gt 0 ] || { echo "usage: $0 DEVICE_UDID..." >&2; exit 2; }
CARGO=${CARGO:-cargo}
TARGET=aarch64-apple-ios
PROFILE=${PROFILE:-release}
[ "$PROFILE" = release ] && FLAG=--release || FLAG=
$CARGO build $FLAG -p lightcraft-mobile --bin lightcraft-ios --target $TARGET
OUT=${CARGO_TARGET_DIR:-target}/$TARGET/$PROFILE
APP=$OUT/LightCraft.app
rm -rf "$APP" && mkdir -p "$APP"
cp "$OUT/lightcraft-ios" "$APP/"
cp apps/lightcraft-mobile/ios/Info.plist "$APP/"
# the app icon, as in ios-sim.sh (for devices: --platform iphoneos)
xcrun actool assets/app-icon/ios/Assets.xcassets --compile "$APP" --platform iphoneos \
    --minimum-deployment-target 17.0 --target-device iphone --target-device ipad --app-icon AppIcon \
    --output-partial-info-plist "$OUT/AppIcon.plist" --output-format human-readable-text --notices --warnings >/dev/null
/usr/libexec/PlistBuddy -c "Merge $OUT/AppIcon.plist" "$APP/Info.plist" >/dev/null
cp "$IOS_PROFILE" "$APP/embedded.mobileprovision"
ENT=$OUT/LightCraft.entitlements
cat > "$ENT" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>application-identifier</key><string>$IOS_TEAM_ID.$BUNDLE_ID</string>
<key>com.apple.developer.team-identifier</key><string>$IOS_TEAM_ID</string>
<key>get-task-allow</key><true/>
</dict></plist>
PLIST
codesign -f -s "$IOS_SIGN_IDENTITY" --entitlements "$ENT" "$APP"
for DEVICE in "$@"; do
    xcrun devicectl device install app --device "$DEVICE" "$APP"
done
