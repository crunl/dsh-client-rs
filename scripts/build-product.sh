#!/bin/bash
# 打一个能直接双击跑的 cowork.app。
#
#   ./scripts/build-product.sh            # 默认 release 构建
#
# 产物：
#   cowork/cowork.app（仓库内产物）
#   /Applications/cowork.app（默认安装位置；COWORK_INSTALL_DIR 可改）
#
# runtime 模式（COWORK_RUNTIME_MODE）：
#   auto     默认；有 COWORK_RUNTIME_PACKAGE_DIR 时 embedded，否则 legacy
#   legacy   旧结构：Resources/runtime/node_modules + node-path.txt
#   embedded 新结构：Resources/runtime-manifest.json + 内嵌 .coworkrt 包
#   plain    新结构：只放 manifest，首次启动按 manifest.url 下载 runtime
#
# COWORK_RUNTIME_PACKAGE_DIR 应指向 scripts/runtime/package-runtime.py 的输出目录。
# 运行时查找顺序见 apps/desktop/src/host.rs。

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="$ROOT/cowork"
APP="$OUT/cowork.app"
STAGING="$ROOT/.runtime-staging"
BIN_NAME="cowork"
RUNTIME_MODE="${COWORK_RUNTIME_MODE:-auto}"
PACKAGE_DIR="${COWORK_RUNTIME_PACKAGE_DIR:-}"
if [ "$RUNTIME_MODE" = auto ]; then
  if [ -n "$PACKAGE_DIR" ]; then RUNTIME_MODE=embedded; else RUNTIME_MODE=legacy; fi
fi

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

RUNTIME_SUMMARY=""
case "$RUNTIME_MODE" in
  legacy)
    echo "==> 内置 dsh 运行时（legacy，APFS 克隆）"
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
      echo "构建机上没有 node：legacy 打包需要本机 node 路径" >&2
      exit 1
    fi
    NODE_BIN="$(cd "$(dirname "$NODE_BIN")" && pwd)/$(basename "$NODE_BIN")"
    printf '%s\n' "$NODE_BIN" > "$APP/Contents/Resources/node-path.txt"
    RUNTIME_SUMMARY="legacy node=$NODE_BIN"
    ;;

  embedded|plain)
    if [ -z "$PACKAGE_DIR" ] || [ ! -f "$PACKAGE_DIR/runtime-manifest.json" ]; then
      echo "COWORK_RUNTIME_PACKAGE_DIR 缺少 runtime-manifest.json" >&2
      exit 1
    fi
    MANIFEST="$PACKAGE_DIR/runtime-manifest.json"
    cp "$MANIFEST" "$APP/Contents/Resources/runtime-manifest.json"
    ARCHIVE="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["archive"])' "$MANIFEST")"
    RUNTIME_ID="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["id"])' "$MANIFEST")"
    if [ "$RUNTIME_MODE" = embedded ]; then
      if [ ! -f "$PACKAGE_DIR/$ARCHIVE" ]; then
        echo "runtime 包不存在：$PACKAGE_DIR/$ARCHIVE" >&2
        exit 1
      fi
      cp "$PACKAGE_DIR/$ARCHIVE" "$APP/Contents/Resources/$ARCHIVE"
    else
      URL="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1])).get("url") or "")' "$MANIFEST")"
      if [ -z "$URL" ]; then
        echo "plain 模式要求 runtime-manifest.json 带 url" >&2
        exit 1
      fi
    fi
    RUNTIME_SUMMARY="$RUNTIME_MODE id=$RUNTIME_ID"
    ;;

  *)
    echo "未知 COWORK_RUNTIME_MODE: $RUNTIME_MODE" >&2
    exit 1
    ;;
esac

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
echo "  runtime: $RUNTIME_SUMMARY"
echo "  profile: cowork（\$DSH_HOME/profiles/cowork，首次启动自动从 web 播种）"
echo "  大小   : $(du -sh "$APP" | cut -f1)"
