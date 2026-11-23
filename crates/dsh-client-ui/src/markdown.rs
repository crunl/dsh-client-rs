//! 消息正文的渲染入口。
//!
//! 第一性原则：**按角色选排版**。assistant 正文与 reasoning 都走 markdown
//! （官方 reasoning 展开体即 compact markdown）；只有 user/system/tool 是普通文本，
//! 解析 markdown 会改变它们的语义。
//!
//! 身份来自数据的 [`RenderId`]，不是渲染顺序。流式块和提交后的同一块共用
//! `(turn, step, index)`，提交那一帧不会重建排版状态。

use dsh_protocol::RenderId;
use gpui_kit::base::text::{MarkdownExtensions, TextView};
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
    // 官方 markdown/render.tsx：raw HTML 永不当元素解析，只作字面文本。
    .markdown_extensions(MarkdownExtensions::default().raw_html(false))
    // 官方 sanitizeUrl：禁 javascript/vbscript/data/blob 与相对地址，保留绝对 URL。
    // link hook 返回 None 时 gpui 把链接当普通文本绘制（不可点、无链接色）。
    .link(|link, _| safe_link_url(&link.url).then(|| link.clone()))
    .into_any_element()
}

/// 链接协议白名单，对应官方 `sanitizeUrl`（render.tsx:55-72）+ `isExplicitFilePath`
/// （file-link.ts:8-10）。
///
/// 顺序与官方一致：先按 scheme trim 后识别显式文件路径并拒绝，再按协议黑名单
/// 判定；只放行带合法 scheme 的绝对 URL。gpui 没有 URL 解析器，按 RFC 3986 的
/// scheme 语法手工判定，不引入新依赖。
pub fn safe_link_url(url: &str) -> bool {
    // WHATWG `new URL()` 会 trim 首尾空白，官方据此判定，-mile 这里对齐避免误杀。
    let url = url.trim();
    if is_explicit_file_path(url) {
        return false;
    }
    let Some(colon) = url.find(':') else {
        return false; // 无 scheme：相对地址 / 锚点 / 协议相对 //host
    };
    let scheme = &url[..colon];
    let mut chars = scheme.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_alphabetic()
        || !chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    {
        return false;
    }
    !matches!(
        scheme.to_ascii_lowercase().as_str(),
        "javascript" | "vbscript" | "data" | "blob"
    )
}

/// 官方 `isExplicitFilePath`（file-link.ts:8-10）正则的等价实现：
/// `^(?:[\\/]|\.{1,2}[\\/]|[a-z]:[\\/])`。拒绝 Windows 盘符路径与 POSIX 绝对/
/// 相对路径——它们不是 URL，交给系统打开器是安全隐患。
fn is_explicit_file_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.is_empty() {
        return false;
    }
    // [\\/]
    if bytes[0] == b'/' || bytes[0] == b'\\' {
        return true;
    }
    // \.{1,2}[\\/]
    if bytes[0] == b'.' {
        let dots = if bytes.get(1) == Some(&b'.') { 2 } else { 1 };
        if matches!(bytes.get(dots), Some(b'/') | Some(b'\\')) {
            return true;
        }
    }
    // [a-z]:[\\/]，官方正则带 /i 标志，故大小写盘符都算本地路径。
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'/' | b'\\')
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

#[cfg(test)]
mod tests {
    use super::safe_link_url;

    #[test]
    fn link_allowlist_accepts_absolute_urls() {
        for url in [
            "https://example.com/a",
            "http://example.com",
            "mailto:someone@example.com",
            "ftp://host/file",
            "custom+scheme.value-1:opaque",
        ] {
            assert!(safe_link_url(url), "应放行绝对 URL：{url}");
        }
    }

    #[test]
    fn link_allowlist_rejects_dangerous_and_relative_urls() {
        for url in [
            "javascript:alert(1)",
            "JavaScript:alert(1)",
            "vbscript:msgbox",
            "data:text/html,<script>",
            "blob:https://example.com/uuid",
            "/relative/path",
            "relative/path",
            "#anchor",
            "//host/protocol-relative",
            "1bad:not-a-scheme",
            ":no-scheme",
            // 官方 isExplicitFilePath：本地路径不是 URL，拒绝交给系统打开器。
            "C:\\Windows\\System32",
            "c:/foo/bar",
            "/absolute/path",
            "\\\\server\\share",
            "../relative",
            "./relative",
        ] {
            assert!(!safe_link_url(url), "应拒绝不安全/相对 URL：{url}");
        }
    }

    #[test]
    fn link_allowlist_trims_surrounding_whitespace() {
        // WHATWG new URL() 会 trim，官方据此放行，避免误杀。
        assert!(safe_link_url("  https://example.com  "));
        assert!(!safe_link_url("  javascript:alert(1)  "));
    }
}
