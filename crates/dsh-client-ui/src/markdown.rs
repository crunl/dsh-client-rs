//! 消息正文的渲染入口。
//!
//! 第一性原则：**解析角色驱动，排版统一**。只有 assistant 正文走 markdown；
//! reasoning 与 user/system/tool 都是普通文本，解析 markdown 会改变语义。
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
    /// assistant reasoning：纯文本 + 弱墨色。
    Reasoning,
    /// 用户/系统文本：纯文本 + 常规墨色。
    Plain,
}

/// 一条正文的视图。
///
/// `COWORK_RICH_TEXT=0` 时 assistant 也退回纯文本（同一入口的降级分支）；
/// 量内存/排查排版问题时用，产品默认走 markdown。
pub fn text(id: RenderId, source: &str, role: TextRole) -> AnyElement {
    if role != TextRole::Assistant || std::env::var("COWORK_RICH_TEXT").is_ok_and(|v| v == "0") {
        return plain(id, source, role);
    }

    TextView::markdown(
        ElementId::Name(id.label().into()),
        SharedString::from(source.to_owned()),
    )
    .font_family(theme::MONO)
    .text_size(px(theme::BODY))
    .style(theme::markdown_style())
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
