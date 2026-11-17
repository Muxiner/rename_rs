#!/usr/bin/env bash
# 一键构建发布产物：
#   1) GUI：macOS .app（可选 .dmg）
#   2) CLI：rename-rust-cli
# 产物统一放到 release/ 目录。
#
# 用法：
#   bash scripts/build-release.sh            # 构建 .app + CLI
#   BUILD_DMG=1 bash scripts/build-release.sh  # 额外构建 .dmg
set -euo pipefail
cd "$(dirname "$0")/.."

APP_NAME="批量重命名工具"
VERSION="$(node -p "require('./package.json').version")"

echo "==> 构建前端"
npm run build

echo "==> 构建 CLI（release）"
(cd src-tauri && cargo build --release --bin rename-rust-cli)

echo "==> 构建 GUI .app（Tauri）"
(cd src-tauri && npx tauri build --bundles app)

mkdir -p release/macos release/bin
cp scripts/release-README.txt release/README.txt
rm -rf "release/macos/${APP_NAME}.app"
cp -R "src-tauri/target/release/bundle/macos/${APP_NAME}.app" "release/macos/"
cp src-tauri/target/release/rename-rust-cli release/bin/
chmod +x release/bin/rename-rust-cli

echo "==> 打包 .app 为 zip"
rm -f "release/macos/${APP_NAME}-${VERSION}-macos.zip"
ditto -c -k --sequesterRsrc --keepParent \
  "release/macos/${APP_NAME}.app" \
  "release/macos/${APP_NAME}-${VERSION}-macos.zip"

if [[ "${BUILD_DMG:-0}" == "1" ]]; then
  echo "==> 构建 .dmg"
  (cd src-tauri && npx tauri build --bundles dmg)
  cp src-tauri/target/release/bundle/dmg/*.dmg release/macos/ 2>/dev/null || true
fi

echo "==> 完成，产物位于 release/"
find release -maxdepth 2 -mindepth 1 -print
