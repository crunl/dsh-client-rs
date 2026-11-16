//! `@` 补全的两块纯逻辑：查什么（`plan_lookup`）与怎么排（`rank`）。
//!
//! 期望值都是从官方 `dsh-file-reference-local/src/search.ts` 反推的，不是自创规则。

use dsh_protocol::files::{
    FileCandidate, FileLookup, has_hidden_segment, is_excluded_path, plan_lookup, rank,
};

fn candidate(path: &str, directory: bool) -> FileCandidate {
    FileCandidate {
        path: path.to_owned(),
        directory,
    }
}

fn paths(candidates: &[FileCandidate]) -> Vec<&str> {
    candidates.iter().map(|one| one.path.as_str()).collect()
}

#[test]
fn bare_at_lists_the_workspace_root() {
    // `@` 一个字符：官方是「列根目录」，不是「等用户再打一个字」。
    assert_eq!(
        plan_lookup(""),
        FileLookup::Directory {
            path: ".".to_owned(),
            fragment: String::new(),
        }
    );
}

#[test]
fn a_short_fragment_searches_the_whole_workspace() {
    assert_eq!(
        plan_lookup("crat"),
        FileLookup::Search("crat".to_owned()),
        "不含 `/` 的片段走全库检索"
    );
}

#[test]
fn a_slash_turns_the_query_into_a_directory_listing() {
    assert_eq!(
        plan_lookup("crates/"),
        FileLookup::Directory {
            path: "crates".to_owned(),
            fragment: String::new(),
        },
        "打完目录再加 `/`，就该列出它的直接子项"
    );
    assert_eq!(
        plan_lookup("crates/dsh-"),
        FileLookup::Directory {
            path: "crates".to_owned(),
            fragment: "dsh-".to_owned(),
        },
        "fragment 留在本地过滤，不再全库跑一遍"
    );
    assert_eq!(
        plan_lookup("/crates"),
        FileLookup::Directory {
            path: ".".to_owned(),
            fragment: "crates".to_owned(),
        },
        "开头的 `/` 当「从根目录开始」：directory 为空就落到根"
    );
}

#[test]
fn an_absolute_directory_inside_the_workspace_can_still_be_drilled() {
    assert_eq!(
        plan_lookup("/Users/x1a2h1/XiaomiMiMoProjects/dsh-client-rs/crates"),
        FileLookup::Directory {
            path: "/Users/x1a2h1/XiaomiMiMoProjects/dsh-client-rs".to_owned(),
            fragment: "crates".to_owned(),
        }
    );
}

#[test]
fn backslashes_are_normalized_before_planning() {
    assert_eq!(
        plan_lookup("crates\\dsh-"),
        FileLookup::Directory {
            path: "crates".to_owned(),
            fragment: "dsh-".to_owned(),
        }
    );
}

#[test]
fn directories_come_first_when_nothing_is_typed() {
    let ranked = rank(
        vec![
            candidate("README.md", false),
            candidate("crates", true),
            candidate(".gitignore", false),
            candidate("apps", true),
        ],
        "",
        8,
    );
    assert_eq!(
        paths(&ranked),
        vec!["apps", "crates", ".gitignore", "README.md"]
    );
}

#[test]
fn an_exact_name_beats_a_prefix_which_beats_a_path_hit() {
    let ranked = rank(
        vec![
            candidate("docs/crates.md", false),
            candidate("crates-lock", false),
            candidate("crates", true),
        ],
        "crates",
        8,
    );
    assert_eq!(
        paths(&ranked),
        vec!["crates", "crates-lock", "docs/crates.md"],
        "名字全等 > 名字前缀 > 路径包含"
    );
}

#[test]
fn at_equal_score_the_shorter_path_wins() {
    let ranked = rank(
        vec![
            candidate("crates/dsh-host/src/main.rs", false),
            candidate("main.rs", false),
        ],
        "main",
        8,
    );
    assert_eq!(
        paths(&ranked),
        vec!["main.rs", "crates/dsh-host/src/main.rs"]
    );
}

#[test]
fn the_limit_cuts_after_ranking() {
    let ranked = rank(
        vec![
            candidate("aaa", false),
            candidate("aab", false),
            candidate("aac", false),
        ],
        "aa",
        2,
    );
    assert_eq!(paths(&ranked), vec!["aaa", "aab"]);
}

#[test]
fn excluded_build_directories_are_recognized_anywhere_in_the_path() {
    for path in [
        "target",
        "target/debug/dsh-desktop",
        "node_modules/react/index.js",
        "crates/dist/x.js",
        ".git/config",
    ] {
        assert!(is_excluded_path(path), "{path} 应该被排除");
    }
    for path in ["crates", "src/lib.rs", "docs/DECISIONS.md", "lib/x.rb"] {
        assert!(!is_excluded_path(path), "{path} 不该被排除");
    }
}

#[test]
fn hidden_segments_are_detected_per_segment() {
    assert!(has_hidden_segment(".gitignore"));
    assert!(has_hidden_segment(".runtime-staging/node_modules"));
    assert!(!has_hidden_segment("crates/dsh-host"));
    assert!(!has_hidden_segment("a.b/c"), "点号不在开头不算隐藏");
}
