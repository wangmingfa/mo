//! 行级 diff 引擎：Myers O(ND) 算法。
//!
//! 把文本按行切成序列、哈希成 `u64` 后跑 Myers 最短编辑脚本，
//! 输出按顺序排列的 [`DiffOp`] 区块序列（相同 / 删除 / 插入）。
//! 渲染层只需顺序遍历区块即可还原出逐行的行号与内容。

/// 一段连续的相同 / 删除 / 插入区块。
///
/// 行号均为 **0 起始**；`Equal` 的 `old`/`new` 是两侧各自的起始行号。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffOp {
    /// 两侧相同的行：旧侧 `old` 起 `count` 行 == 新侧 `new` 起 `count` 行。
    Equal { old: usize, new: usize, count: usize },
    /// 仅旧侧存在的行（被删除）。
    Delete { old: usize, count: usize },
    /// 仅新侧存在的行（新增）。
    Insert { new: usize, count: usize },
}

impl DiffOp {
    /// 该区块覆盖的行数（任一侧取较大值；Equal 两侧相等）。
    pub fn row_count(&self) -> usize {
        match self {
            DiffOp::Equal { count, .. } => *count,
            DiffOp::Delete { count, .. } | DiffOp::Insert { count, .. } => *count,
        }
    }
}

/// 完整的文本 diff：行序列 + 区块序列。
///
/// 持有两侧的行内容（所有权），渲染层无需再读原文件。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TextDiff {
    /// 旧文本按行拆分（与 `str::lines` 语义一致）。
    pub a_lines: Vec<String>,
    /// 新文本按行拆分。
    pub b_lines: Vec<String>,
    /// 按顺序排列的区块序列。
    pub ops: Vec<DiffOp>,
}

/// 计算两段文本的行级 diff。
pub fn text_diff(a: &str, b: &str) -> TextDiff {
    TextDiff {
        a_lines: a.lines().map(|s| s.to_string()).collect(),
        b_lines: b.lines().map(|s| s.to_string()).collect(),
        ops: line_diff(a, b),
    }
}

/// 只计算区块序列（不保留行内容）。
pub fn line_diff(a: &str, b: &str) -> Vec<DiffOp> {
    let a: Vec<u64> = a.lines().map(fnv1a).collect();
    let b: Vec<u64> = b.lines().map(fnv1a).collect();
    diff_hashes(&a, &b)
}

/// FNV-1a 64 位（零依赖；两行不同却撞哈希的概率低到可以忽略）。
fn fnv1a(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in s.bytes() {
        h ^= byte as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// 回溯过程中的单步（区块合并前的原始步骤，逆序收集）。
#[derive(Debug, Clone, Copy)]
enum Step {
    Equal { old: usize, new: usize },
    Delete { old: usize },
    Insert { new: usize },
}

/// Myers 最短编辑脚本（前向 + 回溯）。
///
/// `a` / `b` 为哈希后的行序列；返回按正向顺序排列的 [`DiffOp`] 区块。
fn diff_hashes(a: &[u64], b: &[u64]) -> Vec<DiffOp> {
    let n = a.len();
    let m = b.len();
    if n == 0 && m == 0 {
        return Vec::new();
    }
    if n == 0 {
        return vec![DiffOp::Insert { new: 0, count: m }];
    }
    if m == 0 {
        return vec![DiffOp::Delete { old: 0, count: n }];
    }

    let max = n + m;
    let maxi = max as isize;
    // v[k]：对角线 k 上能到达的最大 x；偏移 maxi 以支持负下标。
    let mut v = vec![0isize; 2 * max + 1];
    // 每轮开始前的 v 快照，供回溯。
    let mut trace: Vec<Vec<isize>> = Vec::with_capacity(max + 1);

    for d in 0..=max {
        trace.push(v.clone());
        let di = d as isize;
        // k 从大到小：决策只读异奇数位，本轮内同奇数位的写入互不影响。
        for k in (-di..=di).rev().step_by(2) {
            let ki = (k + maxi) as usize;
            // 向下（Insert，来自 k+1）或向右（Delete，来自 k-1）。
            let mut x = if k == -di || (k != di && v[ki - 1] < v[ki + 1]) {
                v[ki + 1]
            } else {
                v[ki - 1] + 1
            };
            let mut y = x - k;
            // 沿对角线吃掉所有相同的行。
            while (x as usize) < n && (y as usize) < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            v[ki] = x;
            if x as usize >= n && y as usize >= m {
                return backtrack(&trace, n, m, maxi);
            }
        }
    }
    // 数学上不可达：d = n + m 时必然到达终点。
    unreachable!("Myers 搜索必然在 d <= n+m 内收敛")
}

/// 从 trace 回溯编辑路径，合并成区块序列。
///
/// `n` / `m` 是两侧行数：回溯从终态 `(n, m)` 出发，沿每轮快照倒推每一步。
fn backtrack(trace: &[Vec<isize>], n: usize, m: usize, maxi: isize) -> Vec<DiffOp> {
    let mut x = n as isize;
    let mut y = m as isize;
    let mut steps: Vec<Step> = Vec::new();

    for d in (0..trace.len()).rev() {
        let v = &trace[d];
        let di = d as isize;
        let k = x - y;
        let ki = (k + maxi) as usize;
        // 与前向搜索完全相同的决策：本轮走的是「下」（Insert）还是「右」（Delete）。
        let prev_k = if k == -di || (k != di && v[ki - 1] < v[ki + 1]) {
            k + 1
        } else {
            k - 1
        };
        let prev_x = v[(prev_k + maxi) as usize];
        let prev_y = prev_x - prev_k;

        // 先倒退本轮的对角线段（Equal 行）。
        while x > prev_x && y > prev_y {
            steps.push(Step::Equal {
                old: (x - 1) as usize,
                new: (y - 1) as usize,
            });
            x -= 1;
            y -= 1;
        }
        // 再倒退那一步非对角线移动（d == 0 时路径纯对角线，没有这一步）。
        if d > 0 {
            if x == prev_x {
                steps.push(Step::Insert { new: (y - 1) as usize });
                y -= 1;
            } else {
                steps.push(Step::Delete { old: (x - 1) as usize });
                x -= 1;
            }
        }
    }

    steps.reverse();
    merge_steps(steps)
}

/// 把单步序列合并成区块序列。
fn merge_steps(steps: Vec<Step>) -> Vec<DiffOp> {
    let mut ops: Vec<DiffOp> = Vec::new();
    for s in steps {
        match s {
            Step::Equal { old, new } => match ops.last_mut() {
                Some(DiffOp::Equal { old: o, new: w, count }) if *o + *count == old && *w + *count == new => {
                    *count += 1;
                }
                _ => ops.push(DiffOp::Equal { old, new, count: 1 }),
            },
            Step::Delete { old } => match ops.last_mut() {
                Some(DiffOp::Delete { old: o, count }) if *o + *count == old => {
                    *count += 1;
                }
                _ => ops.push(DiffOp::Delete { old, count: 1 }),
            },
            Step::Insert { new } => match ops.last_mut() {
                Some(DiffOp::Insert { new: w, count }) if *w + *count == new => {
                    *count += 1;
                }
                _ => ops.push(DiffOp::Insert { new, count: 1 }),
            },
        }
    }
    ops
}

#[cfg(test)]
mod tests;
