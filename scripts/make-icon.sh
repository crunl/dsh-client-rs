#!/bin/bash
# 生成 cowork.app 的图标：官方白鲸 mark + 我们的深色 squircle。
#
#   ./scripts/make-icon.sh
#
# 产物：apps/desktop/assets/AppIcon.icns（提交进仓库）
#       apps/desktop/assets/AppIcon.svg（合成源，便于改构图）
#
# 为什么这么做：
# - 图形本身**不自己画**，逐字取自官方前端产物
#   `@deepseek-ai/dsh-web-frontend/dist/favicon-dark.svg`（官方为深色背景准备的白色 mark）。
# - 桌面 UI 全深色（#0F0F10），用官方那张浅底 icon-512.png 会在 Dock 里发一块白板；
#   拿官方白色 mark 放在同一套深色底上，品牌与界面一致。
# - 网格按 macOS Big Sur 规范：1024 画布、824×824 圆角方块、圆角 185、图形约占 60%。
#
# 依赖 rsvg-convert（brew install librsvg）与系统自带 iconutil/sips。

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ASSETS="$ROOT/apps/desktop/assets"
MARK="$ROOT/crates/dsh-client-ui/assets/deepseek-mark-white.svg"
OUT_SVG="$ASSETS/AppIcon.svg"
OUT_ICNS="$ASSETS/AppIcon.icns"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

for candidate in /opt/homebrew/bin/rsvg-convert /usr/local/bin/rsvg-convert; do
  [ -x "$candidate" ] && { RSVG="$candidate"; break; }
done
RSVG="${RSVG:-$(command -v rsvg-convert || true)}"
[ -n "$RSVG" ] || { echo "缺 rsvg-convert：brew install librsvg" >&2; exit 1; }
[ -f "$MARK" ] || { echo "缺官方 mark：$MARK" >&2; exit 1; }

# 从官方 mark 里取出 path 数据（那个 `<path id="path" d="..."/>`）。
# 注意要取最后一个 ` d="`：`id="path"` 里也含 `d="`。
PATH_D="$(sed -n 's/.* d="\([^"]*\)".*/\1/p' "$MARK" | head -1)"
[ ${#PATH_D} -gt 500 ] || { echo "官方 mark 里没取到 path（拿到 ${#PATH_D} 字节）" >&2; exit 1; }

# 图形在官方 mark 里的实际包围盒（把 mark 渲染成 500×500 后用 alpha 扫出来的）：
#   x 10..493  y 70..433  →  单位坐标系里 x 1.0..49.3  y 7.0..43.3
# 它并不在 50×50 的中心（横向偏右、纵向偏下），所以不能直接平移 25,25。
MARK_CX=25.15
MARK_CY=25.15
MARK_W=48.4
MARK_H=36.4
TILE=824            # macOS Big Sur 网格：1024 画布里的 824 圆角方块
ART_RATIO=0.47      # 图形占方块高度的比例（系统图标大致在 0.5–0.6）
read -r SCALE TX TY <<EOF
$(awk -v tile="$TILE" -v ratio="$ART_RATIO" -v h="$MARK_H" -v cx="$MARK_CX" -v cy="$MARK_CY" \
   'BEGIN { s = tile * ratio / h; printf "%.3f %.3f %.3f", s, 512 - s * cx, 512 - s * cy }')
EOF

# 图形按高度定标，宽度得自己卡一下（鲸鱼是横宽的，放大过头会贴到圆角边）。
WIDTH_PX=$(awk -v s="$SCALE" -v w="$MARK_W" 'BEGIN { printf "%.1f", s * w }')
awk -v w="$WIDTH_PX" -v tile="$TILE" 'BEGIN { exit (w <= tile - 60) ? 0 : 1 }' \
  || { echo "图形太宽（${WIDTH_PX}px，可用 $((TILE - 60))px），调小 ART_RATIO" >&2; exit 1; }

mkdir -p "$ASSETS"
cat > "$OUT_SVG" <<SVG
<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024" fill="none">
  <defs>
    <linearGradient id="bg" x1="512" y1="100" x2="512" y2="924" gradientUnits="userSpaceOnUse">
      <stop offset="0" stop-color="#1C1C21"/>
      <stop offset="1" stop-color="#0A0A0C"/>
    </linearGradient>
  </defs>
  <rect x="100" y="100" width="824" height="824" rx="185" fill="url(#bg)"/>
  <rect x="100.5" y="100.5" width="823" height="823" rx="184.5" stroke="#FFFFFF" stroke-opacity="0.07"/>
  <g transform="translate(${TX} ${TY}) scale(${SCALE})">
    <path d="$PATH_D" fill="#F5F5F7"/>
  </g>
</svg>
SVG

echo "==> 光栅化（rsvg-convert）"
mkdir -p "$TMP/AppIcon.iconset"
for spec in "16 16x16" "32 16x16@2x" "32 32x32" "64 32x32@2x" \
            "128 128x128" "256 128x128@2x" "256 256x256" "512 256x256@2x" \
            "512 512x512" "1024 512x512@2x"; do
  set -- $spec
  "$RSVG" -w "$1" -h "$1" -o "$TMP/AppIcon.iconset/icon_$2.png" "$OUT_SVG"
done
# 1024 主体也留下来，Finder 快速查看与 App Store 都用得上
"$RSVG" -w 1024 -h 1024 -o "$ASSETS/AppIcon-1024.png" "$OUT_SVG"

echo "==> 打包 icns"
iconutil -c icns "$TMP/AppIcon.iconset" -o "$OUT_ICNS"
ls -l "$OUT_ICNS" | awk '{print "   " $5 " bytes  " $9}'
