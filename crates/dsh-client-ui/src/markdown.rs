//! 消息正文的渲染入口。
//!
//! 第一性原则：**按角色选排版**。assistant 正文与 reasoning 都走 markdown
//! （官方 reasoning 展开体即 compact markdown）；只有 user/system/tool 是普通文本，
//! 解析 markdown 会改变它们的语义。
//!
//! 身份来自数据的 [`RenderId`]，不是渲染顺序。流式块和提交后的同一块共用
//! `(turn, step, index)`，提交那一帧不会重建排版状态。

use dsh_protocol::RenderId;
use gpui_kit::base::text::TextView;
use gpui_kit::*;

use crate::theme;

/// 正文角色。决定要不要解析 markdown，以及用哪套字色。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextRole {
    /// assistant 正文：markdown + 常规墨色。
    Assistant,
    /// assistant reasoning：compact markdown + 弱墨色（列表/加粗/代码按语义渲染）。
    Reasoning,
    /// 用户/系统文本：纯文本 + 常规墨色。
    Plain,
}

/// 一条正文的视图。
///
/// `COWORK_RICH_TEXT=0` 时 assistant 也退回纯文本（同一入口的降级分支）；
/// 量内存/排查排版问题时用，产品默认走 markdown。
pub fn text(id: RenderId, source: &str, role: TextRole) -> AnyElement {
    let markdown = matches!(role, TextRole::Assistant | TextRole::Reasoning);
    if !markdown || std::env::var("COWORK_RICH_TEXT").is_ok_and(|v| v == "0") {
        return plain(id, source, role);
    }

    let (style, size) = if role == TextRole::Reasoning {
        (theme::reasoning_markdown_style(), theme::BODY - 1.0)
    } else {
        (theme::markdown_style(), theme::BODY)
    };
    TextView::markdown(
        ElementId::Name(id.label().into()),
        SharedString::from(source.to_owned()),
    )
    .font_family(theme::MONO)
    .text_size(px(size))
    .style(style)
    .selectable(true)
    .into_any_element()
}

fn plain(id: RenderId, source: &str, role: TextRole) -> AnyElement {
    let color = if role == TextRole::Reasoning {
        theme::INK_MUTED
    } else {
        theme::INK
    };
    div()
        .id(ElementId::Name(id.label().into()))
        .font_family(theme::MONO)
        .text_size(px(theme::BODY))
        .text_color(rgb(color))
        .child(SharedString::from(source.to_owned()))
        .into_any_element()
}
