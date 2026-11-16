//! 补全触发器：规则对齐官方 `detect.ts` / `grammar.ts` 的边界情形。

use dsh_client_ui::completion::{Active, Kind, accept, command_insert, detect, file_insert};

fn at(query: &str, start: usize, end: usize) -> Active {
    Active {
        kind: Kind::File,
        query: query.to_owned(),
        start,
        end,
        quoted: false,
    }
}

fn slash(query: &str, start: usize, end: usize) -> Active {
    Active {
        kind: Kind::Command,
        query: query.to_owned(),
        start,
        end,
        quoted: false,
    }
}

#[test]
fn slash_opens_at_start_and_after_space() {
    assert_eq!(detect("/per", 4), Some(slash("per", 0, 4)));
    assert_eq!(detect("hi /per", 7), Some(slash("per", 3, 7)));
    assert_eq!(detect("line\n/co", 8), Some(slash("co", 5, 8)));
}

#[test]
fn slash_stays_dead_inside_words_and_urls() {
    // 词中间：不触发
    assert_eq!(detect("a/per", 5), None);
    // 协议分隔符：https:/
    assert_eq!(detect("https:/", 7), None);
    // 双斜杠：只有第二个斜杠死掉，第一个仍算触发（官方同此）
    assert_eq!(detect("//", 2), Some(slash("/", 0, 2)));
    // 已经打过空白，token 结束
    assert_eq!(detect("/per mi", 7), None);
}

#[test]
fn at_needs_a_boundary() {
    assert_eq!(detect("@src", 4), Some(at("src", 0, 4)));
    // 邮箱不是触发点
    assert_eq!(detect("me@host", 7), None);
    // 空白之后可以（caret 在末尾）
    assert_eq!(detect("看下 @sr", 10), Some(at("sr", 7, 10)));
}

#[test]
fn quoted_file_token_keeps_its_quote() {
    let active = detect("@\"my dir", 8).unwrap();
    assert!(active.quoted);
    assert_eq!(active.query, "my dir");

    // 目录保持引号不闭合，文件补闭合
    assert_eq!(file_insert("my dir", true, true).unwrap(), "@\"my dir/");
    assert_eq!(file_insert("my dir", false, true).unwrap(), "@\"my dir\"");
    assert_eq!(
        file_insert("a b/c.txt", false, false).unwrap(),
        "@\"a b/c.txt\""
    );
    assert_eq!(
        file_insert("src/main.rs", false, false).unwrap(),
        "@src/main.rs"
    );
    // 引号本身进不了这个语法
    assert!(file_insert("a\"b", false, false).is_none());
}

#[test]
fn accept_replaces_only_the_token() {
    let text = "看下 @src/ma 然后";
    let active = detect(text, 14).unwrap();
    assert_eq!(active.query, "src/ma");
    // start 是字节偏移：两个汉字各 3 字节，再一个空格。
    assert_eq!(active.start, 7);

    let (next, caret) = accept(text, &active, "@src/main.rs ");
    assert_eq!(next, "看下 @src/main.rs  然后");
    assert_eq!(caret, "看下 @src/main.rs ".len());
}

#[test]
fn command_insert_leaves_a_trailing_space() {
    assert_eq!(command_insert("compact"), "/compact ");
}
