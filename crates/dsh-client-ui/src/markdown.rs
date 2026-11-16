//! 助手正文的 markdown 渲染。
//!
//! 第一性原则：解析是纯函数，排版是状态。所以这里只做两件事——把源文交给
//! `gpui-base` 的 `TextView`（它内部按 `ElementId` 缓存排版状态），以及把
//! 墨色/字号压到 `theme` 这一套上。
//!
//! **身份来自数据，不来自渲染顺序**：`ElementId` 用 `MessageId`（`(turn, step)`
//! 优先，否则事件 `seq`）。流式正在写的那一条与提交后的持久消息在两侧都能拿到
//! 同一个 `(turn, step)`，所以提交那一帧不会重建排版、不会闪。
//!
//! 工具输出**不走这里**：它是字节流，解析 markdown 会改变语义（`*`、diff 的 `-`）。

use dsh_protocol::MessageId;
use gpui_kit::base::text::TextView;
use gpui_kit::*;

use crate::theme;

/// 一条助手消息的正文视图。
///
/// `COWORK_RICH_TEXT=0` 退回纯文本（同一入口的降级分支，不是第二套渲染）：
/// 量内存/排查排版问题时用，产品默认走 markdown。
pub fn text(id: MessageId, source: &str) -> AnyElement {
    if std::env::var("COWORK_RICH_TEXT").is_ok_and(|value| value == "0") {
        return div()
            .font_family(theme::MONO)
            .text_size(px(theme::BODY))
            .text_color(rgb(theme::INK))
            .child(SharedString::from(source.to_owned()))
            .into_any_element();
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
