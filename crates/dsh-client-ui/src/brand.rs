//! 官方 DeepSeek 标识。
//!
//! 取自 harness 自带前端产物里的 `BrandWordmark` 组件：19 条路径（鲸鱼 mark +
//! `deepseek` 字标），坐标就是官方 `viewBox` `0 0 182 24`，逐字照抄——自己重画只会走形。
//! 颜色走 `currentColor`，由 `.text_color()` 投影。

use gpui_kit::*;

/// 锁版宽高比（`viewBox` 182×24）。
const LOCKUP_RATIO: f32 = 182.0 / 24.0;

const LOCKUP_SVG: &[u8] = include_bytes!("../assets/deepseek-lockup.svg");

/// mark + 字标的完整锁版。
pub fn lockup(height: f32, color: u32) -> impl IntoElement {
    svg()
        .data(LOCKUP_SVG)
        .flex_none()
        .h(px(height))
        .w(px(height * LOCKUP_RATIO))
        .text_color(rgb(color))
}
