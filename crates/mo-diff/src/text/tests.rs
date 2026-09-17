//! [`text`] 模块的单测：验证区块序列的正确性与连续性不变量。

use super::*;

/// 不变量：区块序列拼起来必须恰好还原两侧的行序列（行号连续且无缝）。
fn assert_consistent(diff: &TextDiff, a_lines: usize, b_lines: usize) {
    let mut ao = 0;
    let mut bo = 0;
    for op in &diff.ops {
        match op {
            DiffOp::Equal { old, new, count } => {
                assert_eq!(*old, ao, "Equal 区块 old 行号不连续");
                assert_eq!(*new, bo, "Equal 区块 new 行号不连续");
                ao += count;
                bo += count;
                // Equal 区块两侧内容必须真的相等。
                for i in 0..*count {
                    assert_eq!(
                        diff.a_lines[old + i],
                        diff.b_lines[new + i],
                        "Equal 区块内第 {i} 行内容不一致"
                    );
                }
            }
            DiffOp::Delete { old, count } => {
                assert_eq!(*old, ao, "Delete 区块 old 行号不连续");
                ao += count;
            }
            DiffOp::Insert { new, count } => {
                assert_eq!(*new, bo, "Insert 区块 new 行号不连续");
                bo += count;
            }
        }
    }
    assert_eq!(ao, a_lines, "旧侧行数没有耗尽");
    assert_eq!(bo, b_lines, "新侧行数没有耗尽");
}

#[test]
fn identical_texts_are_one_equal_block() {
    let d = text_diff("a\nb\nc", "a\nb\nc");
    assert_eq!(
        d.ops,
        vec![DiffOp::Equal {
            old: 0,
            new: 0,
            count: 3
        }]
    );
    assert_eq!(d.a_lines.len(), 3);
    assert_eq!(d.b_lines.len(), 3);
    assert_consistent(&d, 3, 3);
}

#[test]
fn empty_vs_text_is_pure_insert() {
    let d = text_diff("", "x\ny");
    assert_eq!(d.ops, vec![DiffOp::Insert { new: 0, count: 2 }]);
    assert_consistent(&d, 0, 2);
}

#[test]
fn text_vs_empty_is_pure_delete() {
    let d = text_diff("x\ny", "");
    assert_eq!(d.ops, vec![DiffOp::Delete { old: 0, count: 2 }]);
    assert_consistent(&d, 2, 0);
}

#[test]
fn insertion_in_middle() {
    let d = text_diff("a\nb\nc", "a\nX\nb\nc");
    // 应为：Equal(1) + Insert("X") + Equal(2)。
    assert_eq!(
        d.ops,
        vec![
            DiffOp::Equal {
                old: 0,
                new: 0,
                count: 1
            },
            DiffOp::Insert { new: 1, count: 1 },
            DiffOp::Equal {
                old: 1,
                new: 2,
                count: 2
            },
        ]
    );
    assert_consistent(&d, 3, 4);
}

#[test]
fn deletion_in_middle() {
    let d = text_diff("a\nX\nb\nc", "a\nb\nc");
    assert_eq!(
        d.ops,
        vec![
            DiffOp::Equal {
                old: 0,
                new: 0,
                count: 1
            },
            DiffOp::Delete { old: 1, count: 1 },
            DiffOp::Equal {
                old: 2,
                new: 1,
                count: 2
            },
        ]
    );
    assert_consistent(&d, 4, 3);
}

#[test]
fn modified_line_is_delete_then_insert() {
    let d = text_diff("a\nold\nc", "a\nnew\nc");
    assert_eq!(
        d.ops,
        vec![
            DiffOp::Equal {
                old: 0,
                new: 0,
                count: 1
            },
            DiffOp::Delete { old: 1, count: 1 },
            DiffOp::Insert { new: 1, count: 1 },
            DiffOp::Equal {
                old: 2,
                new: 2,
                count: 1
            },
        ]
    );
    assert_consistent(&d, 3, 3);
}

#[test]
fn completely_different_texts() {
    let d = text_diff("a\nb", "1\n2\n3");
    assert_eq!(
        d.ops,
        vec![
            DiffOp::Delete { old: 0, count: 2 },
            DiffOp::Insert { new: 0, count: 3 },
        ]
    );
    assert_consistent(&d, 2, 3);
}

#[test]
fn trailing_newline_does_not_create_empty_line() {
    let d = text_diff("a\n", "a\n");
    assert_eq!(
        d.ops,
        vec![DiffOp::Equal {
            old: 0,
            new: 0,
            count: 1
        }]
    );
    assert_eq!(d.a_lines, vec!["a".to_string()]);
}

#[test]
fn larger_random_case_stays_consistent() {
    // 构造两段数百行的半随机文本，验证不变量在大输入下成立。
    let mut a = String::new();
    let mut b = String::new();
    for i in 0..300 {
        a.push_str(&format!("line-{i}\n"));
        // 每隔 7 行改一行、每隔 5 行加一行、每隔 11 行删一行。
        if i % 7 == 0 {
            b.push_str(&format!("CHANGED-{i}\n"));
        } else {
            b.push_str(&format!("line-{i}\n"));
        }
        if i % 5 == 0 {
            b.push_str(&format!("added-{i}\n"));
        }
        if i % 11 == 0 {
            i.to_string(); // 删除通过跳过 a 的行体现，无需额外操作。
        }
    }
    // 再从 a 里真正删掉一部分行（跳过 i % 11 == 0 的行）。
    let a2: Vec<&str> = a
        .lines()
        .enumerate()
        .filter(|(i, _)| i % 11 != 0)
        .map(|(_, l)| l)
        .collect();
    let a2 = a2.join("\n");
    let d = text_diff(&a2, b.trim_end());
    assert_consistent(&d, a2.lines().count(), b.trim_end().lines().count());
}

#[test]
fn unicode_lines_are_handled() {
    let d = text_diff("你好\n世界", "你好\n世界！");
    assert_eq!(
        d.ops,
        vec![
            DiffOp::Equal {
                old: 0,
                new: 0,
                count: 1
            },
            DiffOp::Delete { old: 1, count: 1 },
            DiffOp::Insert { new: 1, count: 1 },
        ]
    );
    assert_eq!(d.b_lines[1], "世界！");
}
