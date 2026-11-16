//! 排版回归探针：把一段覆盖全要素的 markdown 直接丢给产品用的渲染入口
//! （`dsh_client_ui::markdown::text`），确认排版与墨色没有被后续改动打坏。
//!
//! ```text
//! cargo run -p dsh-client-ui --profile probe --example markdown_window
//! ```

use dsh_client_ui::markdown;
use dsh_protocol::MessageId;
use gpui_kit::*;

const SAMPLE: &str = r#"## 三个目录名

先 `ls` 一下，**粗体**与 *斜体*、~~删除线~~，还有[一个链接](https://example.com)。

1. **dsh-client-rs** —— 主仓
2. `outputs`
3. qoder-tab-patch
   - 嵌套一项
   - 嵌套二项

- [x] 已完成的待办
- [ ] 未完成的待办

```rust
fn main() {
    let dirs = std::fs::read_dir("/Users/x1a2h1").unwrap();
    println!("{}", dirs.count());
}
```

> 引用：统计方式为 `ls -l | grep -c '^d'`，只计一级子目录。

| 目录 | 类型 | 备注 |
| --- | --- | --- |
| dsh-client-rs | dir | 有 `Cargo.toml` |
| works | dir | 空 |

---

最后一段普通正文，用来确认行高与墨色跟现有时间线一致。
"#;

struct Probe;

impl Render for Probe {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .bg(rgb(0x0f0f10))
            .p(px(28.))
            .child(markdown::text(MessageId::Turn { turn: 0, step: 0 }, SAMPLE))
    }
}

fn main() {
    application().run(|cx| {
        init(cx);
        open_window(WindowOptions::default(), cx, |_, cx| cx.new(|_| Probe))
            .expect("failed to open window");
    });
}
