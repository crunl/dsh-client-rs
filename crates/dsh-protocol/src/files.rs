//! 工作区文件（`workspaceFiles/*`）：`@` 补全的两个数据源与本地排序。
//!
//! 官方 `@` 补全（`dsh-file-reference-local`）不是一个接口打天下，而是按查询分成两种：
//!
//! - **空查询或带 `/` 的查询**：列目录（`workspaceFiles/list`），只给直接子项；
//! - **不带 `/` 的片段**：全工作区排序（本地实现是自建索引，我们用 host 的
//!   `workspaceFiles/search`，结果是 ripgrep 的「按深度再按路径」序，所以要按
//!   官方的打分重新排一次）。
//!
//! 另外三件事也照官方：排除那些「肯定不是源码」的目录名、默认不露隐藏项、
//! 目录排在文件前面。`@` 一个字符就能列出当前目录，靠的就是第一条。

use serde::{Deserialize, Serialize};

/// 不参与补全的目录名（官方 `DEFAULT_FILE_SEARCH_EXCLUDED_DIRECTORIES` 的照抄）。
///
/// 只看 basename，不看层级——`crates/node_modules/x` 整支都不给。
pub const EXCLUDED_DIRECTORIES: &[&str] = &[
    ".git",
    "node_modules",
    "dist",
    "build",
    "out",
    "coverage",
    "target",
    ".next",
    ".nuxt",
    ".turbo",
    ".venv",
    "__pycache__",
    ".pytest_cache",
    ".mypy_cache",
    ".gradle",
];

/// 路径里有没有被排除的目录段。
pub fn is_excluded_path(path: &str) -> bool {
    path.split('/')
        .any(|segment| EXCLUDED_DIRECTORIES.contains(&segment))
}

/// 路径里有没有隐藏段（`.name`）。
pub fn has_hidden_segment(path: &str) -> bool {
    path.split('/').any(|segment| segment.starts_with('.'))
}

/// 一次 `@` 查询该走哪个接口。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileLookup {
    /// 列目录：`workspaceFiles/list`，只给直接子项，`fragment` 在本地过滤。
    Directory {
        /// 工作区相对路径；根目录是 `.`。
        path: String,
        /// 最后一个 `/` 之后的部分（可能为空）。
        fragment: String,
    },
    /// 全工作区模糊检索：`workspaceFiles/search`。
    Search(String),
}

/// 把输入框里 `@` 之后的那段文本翻译成一个请求。
///
/// 规则完全照官方 `WorkspaceFileSearch::list`：空查询、或者带 `/` 都算「列目录」；
/// 不带 `/` 的片段才全库搜。这样 `@` 一敲下去就有当前目录的东西，`@src/` 也能
/// 立刻钻进去，不用多打一个字。
pub fn plan_lookup(query: &str) -> FileLookup {
    let query = query.replace('\\', "/");
    match query.rfind('/') {
        None if query.is_empty() => FileLookup::Directory {
            path: ".".to_owned(),
            fragment: String::new(),
        },
        None => FileLookup::Search(query),
        Some(slash) => {
            let path = &query[..slash];
            FileLookup::Directory {
                path: if path.is_empty() {
                    ".".to_owned()
                } else {
                    path.to_owned()
                },
                fragment: query[slash + 1..].to_owned(),
            }
        }
    }
}

/// 一个候选：工作区相对路径 + 是不是目录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileCandidate {
    /// 工作区相对路径（根目录下就是名字本身）。
    pub path: String,
    /// 目录为真（排前面，且接受后不闭合补全）。
    pub directory: bool,
}

/// 按官方的打分排序并截到 `limit`。
///
/// 分数分层（严格先后）：名字全等 > 名字前缀 > 名字包含 > 路径包含 > 子序列；
/// 目录额外 +25；同分时目录在前，然后（非空查询）路径短的在前，最后按字典序。
pub fn rank(candidates: Vec<FileCandidate>, fragment: &str, limit: usize) -> Vec<FileCandidate> {
    let needle = fragment.to_lowercase();
    let mut scored: Vec<(i32, FileCandidate)> = candidates
        .into_iter()
        .filter_map(|candidate| {
            let score = score_candidate(&candidate, &needle)?;
            Some((score, candidate))
        })
        .collect();
    scored.sort_by(|(left_score, left), (right_score, right)| {
        right_score
            .cmp(left_score)
            .then_with(|| right.directory.cmp(&left.directory))
            .then_with(|| {
                if needle.is_empty() {
                    std::cmp::Ordering::Equal
                } else {
                    left.path.len().cmp(&right.path.len())
                }
            })
            .then_with(|| left.path.cmp(&right.path))
    });
    scored
        .into_iter()
        .take(limit)
        .map(|(_, candidate)| candidate)
        .collect()
}

fn score_candidate(candidate: &FileCandidate, needle: &str) -> Option<i32> {
    if needle.is_empty() {
        return Some(0);
    }
    let path = candidate.path.to_lowercase();
    let name = path.rsplit('/').next().unwrap_or(path.as_str());
    let bonus = if candidate.directory { 25 } else { 0 };
    if name == needle {
        return Some(1_000 + bonus);
    }
    if name.starts_with(needle) {
        return Some(900 + bonus);
    }
    if name.contains(needle) {
        return Some(700 + bonus);
    }
    if path.contains(needle) {
        return Some(500 + bonus);
    }
    subsequence_score(&path, needle).map(|score| 300 + score + bonus)
}

/// 子序列匹配：按命中间距罚分，最多 100。
fn subsequence_score(target: &str, needle: &str) -> Option<i32> {
    let mut from = 0;
    let mut gap = 0usize;
    for character in needle.chars() {
        let rest = target.get(from..)?;
        let at = rest.find(character)?;
        gap += rest[..at].chars().count();
        from += at + character.len_utf8();
    }
    Some((100i32 - i32::try_from(gap).unwrap_or(i32::MAX)).max(0))
}

/// 一次检索的入参。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileSearchRequest {
    /// 大小写不敏感的子串；含 `/` 时匹配工作区相对路径，否则匹配条目名。
    pub query: String,
    /// 字面排除项（按 rg glob 转义）。
    pub excluded_paths: Vec<String>,
    /// 是否包含隐藏文件；VCS 元数据永远排除。
    pub include_hidden: bool,
    /// 是否遵守原生 ignore 规则（`.gitignore` / `.ignore` 等）。
    pub respect_gitignore: bool,
}

impl FileSearchRequest {
    /// 补全用默认值：不排除、不含隐藏、遵守 ignore。
    pub fn new(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            excluded_paths: Vec::new(),
            include_hidden: false,
            respect_gitignore: true,
        }
    }
}

/// 一个命中条目。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileSearchMatch {
    /// 工作区相对路径，永远非空。
    pub path: String,
    /// 条目类型（`file` / `directory` / …）。
    #[serde(rename = "type")]
    pub kind: Option<String>,
}

/// 检索结果。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileSearchResult {
    /// 命中项，按深度再按路径排序，已截到上限。
    #[serde(default)]
    pub matches: Vec<FileSearchMatch>,
    /// 命中数是否超过上限被截断。
    #[serde(default)]
    pub truncated: bool,
    /// 文件集本身是否被截断（扫描不完整）。
    #[serde(default)]
    pub incomplete: bool,
}

/// `workspaceFiles/list` 的一个直接子项。
#[derive(Debug, Clone, Deserialize)]
pub struct DirectoryEntry {
    /// 目录内的基名。
    pub name: String,
    /// `file` / `directory` / `other`（符号链接报目标类型）。
    #[serde(rename = "type")]
    pub kind: String,
    /// 普通文件才有字节数。
    #[serde(default)]
    pub size: Option<u64>,
}

/// `workspaceFiles/list` 的结果：一个目录的直接子项。
///
/// `path` 是相对工作区根的路径（根自己就是空串），子项的路径就是把 `path` 与
/// `name` 用 `/` 拼起来。
#[derive(Debug, Clone, Deserialize)]
pub struct DirectoryListing {
    /// 被枚举的目录。
    #[serde(default)]
    pub path: String,
    /// 子项，按 host 的稳定名字序，已截到上限。
    #[serde(default)]
    pub entries: Vec<DirectoryEntry>,
    /// 是否被条目上限截断。
    #[serde(default)]
    pub truncated: bool,
}
