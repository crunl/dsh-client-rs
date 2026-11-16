#!/bin/bash
# 打一个能直接双击跑的 cowork.app。
#
#   ./scripts/build-product.sh            # 默认 release 构建
#
# 产物：
#   cowork/cowork.app（仓库内产物）
#   /Applications/cowork.app（默认安装位置；COWORK_INSTALL_DIR 可改）
#     Contents/MacOS/cowork                 ← target/release/cowork
#     Contents/Resources/runtime/node_modules  ← dsh 运行时（APFS 克隆，秒级）
#     Contents/Resources/node-path.txt      ← 构建时找到的 node 绝对路径
#
# 为什么要记 node 的绝对路径：从 Finder 双击的 app 拿不到 shell 的 PATH
# （只有 /usr/bin:/bin:/usr/sbin:/sbin），`Command::new("node")` 会失败。
# 运行时的查找顺序见 apps/desktop/src/host.rs。

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="$ROOT/cowork"
APP="$OUT/cowork.app"
STAGING="$ROOT/.runtime-staging"
BIN_NAME="cowork"

cd "$ROOT"

echo "==> cargo build --release -p dsh-desktop"
cargo build --release -p dsh-desktop

echo "==> 组装 $APP"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

cp "target/release/$BIN_NAME" "$APP/Contents/MacOS/$BIN_NAME"
chmod +x "$APP/Contents/MacOS/$BIN_NAME"

cat > "$APP/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key>
  <string>cowork</string>
  <key>CFBundleDisplayName</key>
  <string>cowork</string>
  <key>CFBundleIdentifier</key>
  <string>dev.cowork.desktop</string>
  <key>CFBundleExecutable</key>
  <string>cowork</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>CFBundleShortVersionString</key>
  <string>0.1.0</string>
  <key>CFBundleIconFile</key>
  <string>AppIcon</string>
  <key>CFBundleVersion</key>
  <string>1</string>
  <key>LSMinimumSystemVersion</key>
  <string>13.0</string>
  <key>NSHighResolutionCapable</key>
  <true/>
  <key>NSSupportsAutomaticTermination</key>
  <false/>
</dict>
</plist>
PLIST
printf 'APPL????' > "$APP/Contents/PkgInfo"

# 图标：官方白鲸 mark + 深色 squircle，由 scripts/make-icon.sh 生成（见 DECISIONS #46）。
ICNS="$ROOT/apps/desktop/assets/AppIcon.icns"
if [ -f "$ICNS" ]; then
  cp "$ICNS" "$APP/Contents/Resources/AppIcon.icns"
else
  echo "缺图标 $ICNS：先跑 ./scripts/make-icon.sh" >&2
  exit 1
fi

echo "==> 内置 dsh 运行时（APFS 克隆）"
if [ ! -f "$STAGING/node_modules/@deepseek-ai/dsh/lib/bin.js" ]; then
  echo "缺少 .runtime-staging：先跑 npm install --prefix .runtime-staging" >&2
  exit 1
fi
mkdir -p "$APP/Contents/Resources/runtime"
cp -Rc "$STAGING/node_modules" "$APP/Contents/Resources/runtime/node_modules" 2>/dev/null \
  || cp -R "$STAGING/node_modules" "$APP/Contents/Resources/runtime/node_modules"
cp "$STAGING/package.json" "$APP/Contents/Resources/runtime/package.json"

NODE_BIN="$(command -v node || true)"
if [ -z "$NODE_BIN" ]; then
  echo "构建机上没有 node：打包后运行时找不到解释器" >&2
  exit 1
fi
NODE_BIN="$(cd "$(dirname "$NODE_BIN")" && pwd)/$(basename "$NODE_BIN")"
printf '%s\n' "$NODE_BIN" > "$APP/Contents/Resources/node-path.txt"

echo "==> ad-hoc 签名"
codesign --force --deep --sign - "$APP" >/dev/null

echo "==> 安装到 ${COWORK_INSTALL_DIR:-/Applications}"
INSTALL_DIR="${COWORK_INSTALL_DIR:-/Applications}"
DEST="$INSTALL_DIR/cowork.app"
mkdir -p "$INSTALL_DIR"
if [ -d "$DEST" ]; then
  # 旧安装进废纸篓（可恢复），不直接 rm。
  mv "$DEST" "$HOME/.Trash/cowork.app.$(date +%Y%m%d-%H%M%S)"
fi
# ditto 保留 bundle 元数据与签名；APFS 上同卷克隆很快。
ditto "$APP" "$DEST"

echo
echo "完成：$APP"
echo "  安装   : $DEST"
echo "  node   : $NODE_BIN"
echo "  profile: cowork（\$DSH_HOME/profiles/cowork，首次启动自动从 web 播种）"
echo "  大小   : $(du -sh "$APP" | cut -f1)"
