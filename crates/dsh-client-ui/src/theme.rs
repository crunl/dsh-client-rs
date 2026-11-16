//! fx-ui 视觉令牌，取自 Ardot 设计稿 `747373011992944`（`fx-ui style · dsh-client-rs`）。
//!
//! 规则：mono 贯穿全文、层级靠字色、chrome 零彩色。
//! 设计稿的文字是 11/12/13/14px 四档；数字全是实测几何，不要凭感觉改。

/// 主区底色。
pub const BG_MAIN: u32 = 0x0f0f10;
/// 侧栏底色（比主区更沉）。
pub const BG_SIDEBAR: u32 = 0x0a0a0b;
/// 选中行底色。
pub const BG_ACTIVE: u32 = 0x232326;

/// 行 hover 底色：比侧栏亮一档，比选中态暗。
pub const BG_HOVER: u32 = 0x1a1a1d;
/// composer 卡片的外沿（靠 1px 内边距做出发丝线）。
pub const BG_CARD_EDGE: u32 = 0x27272a;
/// composer 卡片内层。
pub const BG_CARD: u32 = 0x161618;

/// 一级文字。
pub const INK: u32 = 0xededed;
/// 二级文字（标签、路径、chips）。
pub const INK_SOFT: u32 = 0xa1a1a6;
/// 三级文字（计数、分隔符、图标）。
pub const INK_MUTED: u32 = 0x56565b;

/// ctx 圆环的底槽。比卡片边框亮一档，深色主区上也看得见。
pub const RING_TRACK: u32 = 0x34343a;
/// 连接状态点。
pub const OK: u32 = 0x6cc58a;

/// 等宽字体族。设计稿用 Inter，但 fx-ui 规定正文 mono（DECISIONS #7）。
pub const MONO: &str = "Menlo";

/// 设计稿正文。
pub const BODY: f32 = 14.0;
/// 界面标签。
pub const UI: f32 = 13.0;
/// 侧栏会话行。
pub const ROW: f32 = 12.0;
/// 元信息。
pub const META: f32 = 11.0;

/// 侧栏宽度（设计稿实测 264）。
pub const SIDEBAR_W: f32 = 264.0;
/// 内容列宽度（时间线与 composer 同为 860）。
pub const CONTENT_W: f32 = 860.0;

/// 右侧面板宽度。
pub const PANEL_W: f32 = 300.0;
/// 侧栏顶/底与主区标题栏高度。
pub const BAR_H: f32 = 52.0;
/// 主区水平内边距。
pub const GUTTER: f32 = 28.0;
/// 侧栏内边距。
pub const SIDE_PAD: f32 = 16.0;

/// 助手正文（markdown）的排版样式。
///
/// 全部取值就是本文件前面的墨色常量：正文 `INK`、次要 `INK_SOFT`、弱 `INK_MUTED`，
/// 代码块/行内码/表格底用 `BG_CARD`，边线 `BG_CARD_EDGE`。
/// 开 `with_dark(true)` 是必要的：`TextViewStyle::default()` 是浅色 token，
/// 不改它，行内码与表格会是白底黑字。
///
/// 语法制高亮不接（tree-sitter 常驻内存与 north star 冲突），链接点击默认
/// 走 `App::open_url`，不需要 handler。
pub fn markdown_style() -> gpui_kit::base::text::TextViewStyle {
    use gpui_kit::{HighlightStyle, StyleRefinement, Styled as _, rgb};

    gpui_kit::base::text::TextViewStyle::default()
        .with_dark(true)
        .with_foreground(rgb(INK).into())
        .with_muted_foreground(rgb(INK_MUTED).into())
        .with_link(rgb(INK_SOFT).into())
        .with_code_background(rgb(BG_CARD).into())
        .with_border(rgb(BG_CARD_EDGE).into())
        .with_inline_code(HighlightStyle {
            color: Some(rgb(INK).into()),
            background_color: Some(rgb(BG_CARD).into()),
            ..Default::default()
        })
        .with_table(
            StyleRefinement::default()
                .bg(rgb(BG_CARD))
                .border_color(rgb(BG_CARD_EDGE)),
        )
        .with_table_head(
            StyleRefinement::default()
                .bg(rgb(BG_CARD))
                .text_color(rgb(INK_SOFT)),
        )
        .with_table_cell(
            StyleRefinement::default()
                .bg(rgb(BG_CARD))
                .text_color(rgb(INK)),
        )
        .with_table_row_hover(rgb(BG_CARD_EDGE).into())
}
