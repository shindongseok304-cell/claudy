#!/bin/zsh
# Claudy.app 만들기 (macOS). 사용: tools/build_mac.sh
# 결과: dist/Claudy.app (ad-hoc 서명)
set -euo pipefail
cd "$(dirname "$0")/.."
VER=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)

# 실행 파일에 빌드한 컴퓨터의 사용자 폴더 경로가 들어가지 않게 한다
export RUSTFLAGS="--remap-path-prefix=$HOME=/build"
cargo build --release

APP=dist/Claudy.app
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/ClaudyManager "$APP/Contents/MacOS/Claudy"

# 아이콘: res/app.ico 의 가장 큰 그림으로 icns 를 만든다
TMP=$(mktemp -d)
sips -s format png res/app.ico --out "$TMP/base.png" >/dev/null
mkdir "$TMP/Claudy.iconset"
for s in 16 32 128 256 512; do
  sips -z $s $s "$TMP/base.png" --out "$TMP/Claudy.iconset/icon_${s}x${s}.png" >/dev/null
  d=$((s*2))
  sips -z $d $d "$TMP/base.png" --out "$TMP/Claudy.iconset/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns "$TMP/Claudy.iconset" -o "$APP/Contents/Resources/Claudy.icns"
rm -rf "$TMP"

cat > "$APP/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Claudy</string>
  <key>CFBundleDisplayName</key><string>Claudy</string>
  <key>CFBundleIdentifier</key><string>com.claudy.manager</string>
  <key>CFBundleExecutable</key><string>Claudy</string>
  <key>CFBundleIconFile</key><string>Claudy</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>${VER}</string>
  <key>CFBundleVersion</key><string>${VER}</string>
  <key>CFBundleDevelopmentRegion</key><string>ko</string>
  <key>LSMinimumSystemVersion</key><string>12.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
</dict>
</plist>
EOF

codesign --force --deep --sign - "$APP"
echo "완료: $APP ($VER)"
