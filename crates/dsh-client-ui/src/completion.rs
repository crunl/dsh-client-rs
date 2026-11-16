//! composer 的 `@` / `/` 补全：触发器识别 + 接受后的插入文本。
//!
//! 规则逐条对齐官方两处纯函数，不做自己的发明：
//! - `dsh-client-ui-input-trigger/src/core/detect.ts`（触发与词边界）
//! - `dsh-file-reference/src/grammar.ts`（`@` token 语法与插入格式）
//!
//! 只在光标处识别一个 token；不接受就什么都不改。

/// 补全种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `/`：斜杠命令。
    Command,
    /// `@`：工作区文件。
    File,
}

/// 光标处一个活着的触发 token。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Active {
    /// 哪一类。
    pub kind: Kind,
    /// 用户已经打出的查询串（不含触发字符）。
    pub query: String,
    /// 待替换区间的起点（字节偏移，含触发字符）。
    pub start: usize,
    /// 待替换区间的终点（字节偏移，等于光标）。
    pub end: usize,
    /// `@` 是否用引号起的头（`@"…`），接受时要保持引号。
    pub quoted: bool,
}

impl Active {
    /// 查询串是否为空——空查询通常要显示「全部候选」。
    pub fn is_blank(&self) -> bool {
        self.query.is_empty()
    }
}

/// 判断 `index` 处的 `/` 能不能开启补全。
///
/// 官方口径：草稿开头、空白之后、或标点之后才算；`https://` 这类 URL 里的
/// 斜杠不算——`//` 的第二个斜杠、以及紧跟非空白字符的 `:` 之后的斜杠都排除。
fn boundary_ok(text: &str, index: usize) -> bool {
    if index == 0 {
        return true;
    }
    let Some(prev) = text[..index].chars().next_back() else {
        return true;
    };
    if prev.is_whitespace() {
        return true;
    }
    if prev.is_alphanumeric() || prev == '_' {
        return false;
    }
    if prev == '/' {
        return false;
    }
    if prev == ':' {
        let before_colon = text[..index - 1].chars().next_back();
        if let Some(earlier) = before_colon
            && !earlier.is_whitespace()
        {
            return false;
        }
    }
    true
}

/// 在光标处识别触发 token；不在 token 里就返回 `None`。
pub fn detect(text: &str, caret: usize) -> Option<Active> {
    let caret = caret.min(text.len());
    if !text.is_char_boundary(caret) {
        return None;
    }

    if let Some(active) = detect_file(text, caret) {
        return Some(active);
    }
    detect_command(text, caret)
}

/// `@path` / `@"path with spaces`：`@` 必须在开头或空白之后（邮箱不触发）。
///
/// 与官方 `activeAtToken` 的两条正则等价：先试带引号的（引号内允许空白），
/// 再试不带引号的（到空白为止）。
fn detect_file(text: &str, caret: usize) -> Option<Active> {
    let before = &text[..caret];

    // 带引号：最近的 `"` 若是被 `@` 引出来的，且中间再没有别的引号，就是活 token。
    if let Some(quote) = before.rfind('"')
        && let Some(at) = quote.checked_sub(1)
        && before[..quote].ends_with('@')
        && (at == 0
            || before[..at]
                .chars()
                .next_back()
                .is_some_and(char::is_whitespace))
    {
        let inner = &before[quote + 1..];
        if !inner.contains('"') {
            return Some(Active {
                kind: Kind::File,
                query: inner.to_owned(),
                start: at,
                end: caret,
                quoted: true,
            });
        }
    }

    // 不带引号：从光标往前找到最近的 `@`，中途遇到空白就说明 token 已经结束。
    let mut found = None;
    for (index, ch) in before.char_indices().rev() {
        if ch == '@' {
            found = Some(index);
            break;
        }
        if ch.is_whitespace() || ch == '"' {
            return None;
        }
    }
    let index = found?;
    if index > 0 {
        let prev = before[..index].chars().next_back()?;
        if !prev.is_whitespace() {
            return None;
        }
    }
    Some(Active {
        kind: Kind::File,
        query: before[index + 1..].to_owned(),
        start: index,
        end: caret,
        quoted: false,
    })
}

/// `/command`：从光标往前扫到空白，遇到能开启补全的斜杠就算命中。
fn detect_command(text: &str, caret: usize) -> Option<Active> {
    for (index, ch) in text[..caret].char_indices().rev() {
        if ch.is_whitespace() {
            return None;
        }
        if ch != '/' || !boundary_ok(text, index) {
            continue;
        }
        return Some(Active {
            kind: Kind::Command,
            query: text[index + 1..caret].to_owned(),
            start: index,
            end: caret,
            quoted: false,
        });
    }
    None
}

/// 接受一条命令后的插入文本：`/name `（留一个空格好接着打参数）。
pub fn command_insert(name: &str) -> String {
    format!("/{name} ")
}

/// 接受一个文件后的插入文本。
///
/// 与官方 `formatFileMention` 一致：路径含空白就加引号；目录补一个尾斜杠，
/// 而且带引号的目录**不闭合**引号，方便继续往下一层补。
/// 控制字符或引号进不了这个语法，返回 `None` 表示「这条不能插」。
pub fn file_insert(path: &str, is_directory: bool, quoted: bool) -> Option<String> {
    let path = if is_directory {
        format!("{path}/")
    } else {
        path.to_owned()
    };
    if path.chars().any(|ch| ch.is_control() || ch == '"') {
        return None;
    }
    let quoted = quoted || path.chars().any(char::is_whitespace);
    Some(match (quoted, is_directory) {
        (false, _) => format!("@{path}"),
        (true, true) => format!("@\"{path}"),
        (true, false) => format!("@\"{path}\""),
    })
}

/// 用补全结果替换掉触发 token，返回新草稿与应该落到的光标位置。
pub fn accept(text: &str, active: &Active, insert: &str) -> (String, usize) {
    let mut next = String::with_capacity(text.len() + insert.len());
    next.push_str(&text[..active.start]);
    next.push_str(insert);
    next.push_str(&text[active.end..]);
    let caret = active.start + insert.len();
    (next, caret)
}
