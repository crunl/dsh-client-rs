//! 内存基线探针：只开一个空窗口，什么都不画。
//!
//! 用途是给 north star「把 desktop 的 RAM 减少到极致」定标尺，不是产品代码。
//! 外部用 `ps -o rss= -p <pid>` 读稳态 RSS。
//!
//! ```text
//! cargo run --profile probe --example empty_window                         # 仅 gpui-base（默认）
//! cargo run --profile probe --example empty_window --features component   # 带控件库 + 资源包
//! ```

use gpui_kit::*;

struct Empty;

impl Render for Empty {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

fn main() {
    application().run(|cx| {
        init(cx);
        open_window(WindowOptions::default(), cx, |_, cx| cx.new(|_| Empty))
            .expect("failed to open window");
    });
}
