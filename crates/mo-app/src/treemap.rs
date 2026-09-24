//! 磁盘地图（treemap）的布局算法：把一棵「目录树 + 大小」铺成一组互不重叠的矩形。
//!
//! 面积 = 大小，所以「谁吃掉了这块盘」一眼看得见——这是条形图给不了的：条
//! 形图只有一维，十个以上条目就只剩排名，看不出**比例**。
//!
//! 用的是 Bruls / Huizing / van Wijk 的 **squarified** 布局（不断把最长边切
//! 出「长宽比最接近 1」的一行），而不是交替横竖切的 slice-and-dice：后者会
//! 把大目录切成一排细长条，小文件挤成看不见的针。
//!
//! 输出的矩形是**归一化**的（`0..1`，相对整张画布），不是像素。理由很实在：
//! 容器多大只有渲染那帧才知道（受窗口尺寸 / 侧栏开关影响），而布局是纯计算、
//! 要在单测里断言；交给 UI 用 `relative()` 定位，两边就解耦了。

use std::path::PathBuf;

/// 一棵「路径 + 递归大小」的树（磁盘分析的中间产物）。
#[derive(Debug, Clone)]
pub struct UsageTree {
    pub path: PathBuf,
    pub name: String,
    /// 递归总大小（字节）。目录 = 子树之和。
    pub size: u64,
    pub is_dir: bool,
    /// 子项（`max_depth` 到底的目录为空）。
    pub children: Vec<UsageTree>,
}

/// 归一化矩形（相对整张画布）。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    /// 整张画布。
    pub fn unit() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            w: 1.0,
            h: 1.0,
        }
    }

    pub fn area(self) -> f32 {
        self.w * self.h
    }

    /// 与另一矩形的重叠面积（0 = 不重叠）。单测用，也用来兜住「算错了会互相压住」。
    pub fn overlap(self, other: Self) -> f32 {
        let dx = (self.x + self.w).min(other.x + other.w) - self.x.max(other.x);
        let dy = (self.y + self.h).min(other.y + other.h) - self.y.max(other.y);
        if dx <= 0.0 || dy <= 0.0 {
            0.0
        } else {
            dx * dy
        }
    }
}

/// 画布上的一块：要么是文件，要么是不再展开的目录（含「到底了」的目录）。
#[derive(Debug, Clone)]
pub struct Tile {
    pub path: PathBuf,
    pub name: String,
    pub size: u64,
    pub is_dir: bool,
    pub rect: Rect,
    /// 在树里的深度（0 = 被分析目录的直接子项）。配色按它分层。
    pub depth: usize,
}

/// 铺开一棵树，返回全部可见块。
///
/// 目录默认展开到 `max_depth` 层：再深下去块就比一个像素还小，铺了也是噪点，
/// 不如停在那一层、让它整块着色（点击可继续下钻）。
///
/// ⚠️ 大小为 0 的条目**不出现**：它们没有面积，画出来就是零尺寸的矩形。空文件
/// 与空目录在「谁占了空间」这张图里本来也不该占位。
pub fn layout(tree: &UsageTree, max_depth: usize) -> Vec<Tile> {
    let mut out = Vec::new();
    if tree.children.is_empty() {
        // 被分析目录本身就是个空目录（或没有可画的子项）：给它自己一块，
        // 免得整张画布空白得像是没算出来。
        if tree.size > 0 {
            out.push(Tile {
                path: tree.path.clone(),
                name: tree.name.clone(),
                size: tree.size,
                is_dir: tree.is_dir,
                rect: Rect::unit(),
                depth: 0,
            });
        }
        return out;
    }
    layout_into(&tree.children, Rect::unit(), 0, max_depth, &mut out);
    out
}

fn layout_into(
    nodes: &[UsageTree],
    rect: Rect,
    depth: usize,
    max_depth: usize,
    out: &mut Vec<Tile>,
) {
    // 零面积的条目不参与铺放（见 `layout` 的说明）。
    let shown: Vec<&UsageTree> = nodes.iter().filter(|n| n.size > 0).collect();
    if shown.is_empty() || rect.w <= 0.0 || rect.h <= 0.0 {
        return;
    }
    let rects = squarify(
        &shown.iter().map(|n| n.size as f64).collect::<Vec<_>>(),
        rect,
    );
    for (node, r) in shown.into_iter().zip(rects) {
        let can_descend = node.is_dir && !node.children.is_empty() && depth + 1 < max_depth;
        if can_descend {
            layout_into(&node.children, r, depth + 1, max_depth, out);
        } else {
            out.push(Tile {
                path: node.path.clone(),
                name: node.name.clone(),
                size: node.size,
                is_dir: node.is_dir,
                rect: r,
                depth,
            });
        }
    }
}

/// Squarified treemap：把 `sizes` 铺进 `rect`，返回与输入**同序**的矩形。
fn squarify(sizes: &[f64], rect: Rect) -> Vec<Rect> {
    let n = sizes.len();
    let mut out = vec![Rect::default(); n];
    let total: f64 = sizes.iter().sum();
    if total <= 0.0 || rect.w <= 0.0 || rect.h <= 0.0 {
        return out;
    }
    // 面积比例尺：整块 rect 的面积代表 total 字节。
    let scale = (rect.w as f64 * rect.h as f64) / total;
    // 从大到小铺：这是 squarified 的前提（小块补在最后才不会被拉成细条）。
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| {
        sizes[b]
            .partial_cmp(&sizes[a])
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut r = rect;
    let mut i = 0usize;
    while i < order.len() {
        let horizontal = r.w >= r.h;
        let side = if horizontal { r.w as f64 } else { r.h as f64 };
        if side <= 0.0 {
            break;
        }
        // 逐条加进当前行，只要最差长宽比还在变好；一旦变差就收手，另起一行。
        let mut j = i;
        let mut sum = sizes[order[i]];
        let mut best = worst(&order[i..=i], sizes, sum, scale, side);
        while j + 1 < order.len() {
            let next = sum + sizes[order[j + 1]];
            let w = worst(&order[i..=j + 1], sizes, next, scale, side);
            if w > best {
                break;
            }
            best = w;
            sum = next;
            j += 1;
        }
        let thickness = sum * scale / side;
        if thickness <= 0.0 {
            break;
        }
        let mut offset = 0.0f64;
        for k in i..=j {
            let len = sizes[order[k]] * scale / thickness;
            out[order[k]] = if horizontal {
                // 沿竖直方向排一“列”，列宽 = thickness。
                Rect {
                    x: r.x,
                    y: r.y + offset as f32,
                    w: thickness as f32,
                    h: len as f32,
                }
            } else {
                Rect {
                    x: r.x + offset as f32,
                    y: r.y,
                    w: len as f32,
                    h: thickness as f32,
                }
            };
            offset += len;
        }
        // 剩下的画布：沿长边啃掉这一条的厚度。
        r = if horizontal {
            Rect {
                x: r.x + thickness as f32,
                y: r.y,
                w: (r.w - thickness as f32).max(0.0),
                h: r.h,
            }
        } else {
            Rect {
                x: r.x,
                y: r.y + thickness as f32,
                w: r.w,
                h: (r.h - thickness as f32).max(0.0),
            }
        };
        i = j + 1;
    }
    out
}

/// 一行（一列）里最差的长宽比：越小越方正。
fn worst(row: &[usize], sizes: &[f64], sum: f64, scale: f64, side: f64) -> f64 {
    if sum <= 0.0 || side <= 0.0 {
        return f64::MAX;
    }
    let thickness = sum * scale / side;
    if thickness <= 0.0 {
        return f64::MAX;
    }
    let mut mx: f64 = 1.0;
    for &k in row {
        let len = sizes[k] * scale / thickness;
        if len <= 0.0 {
            return f64::MAX;
        }
        mx = mx.max((len / thickness).max(thickness / len));
    }
    mx
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(name: &str, size: u64, children: Vec<UsageTree>) -> UsageTree {
        UsageTree {
            path: PathBuf::from(name),
            name: name.to_string(),
            size: children.iter().map(|c| c.size).sum::<u64>().max(size),
            is_dir: !children.is_empty(),
            children,
        }
    }

    fn root(children: Vec<UsageTree>) -> UsageTree {
        node("root", 0, children)
    }

    /// 面积必须与大小成正比——这是 treemap 唯一的硬契约。
    #[test]
    fn areas_are_proportional_to_sizes() {
        let tree = root(vec![
            node("a", 600, vec![]),
            node("b", 300, vec![]),
            node("c", 100, vec![]),
        ]);
        let tiles = layout(&tree, 3);
        assert_eq!(tiles.len(), 3);
        let total: f64 = tiles.iter().map(|t| t.rect.area() as f64).sum();
        assert!((total - 1.0).abs() < 1e-3, "整张画布应被铺满：{total}");
        for t in &tiles {
            let want = t.size as f64 / 1000.0;
            let got = t.rect.area() as f64;
            assert!(
                (got - want).abs() < 1e-3,
                "{} 的面积 {got} 应当等于占比 {want}",
                t.name
            );
        }
    }

    /// 铺出来的块不能互相压住。
    #[test]
    fn tiles_never_overlap() {
        let tree = root(
            (0..12)
                .map(|i| node(&format!("n{i}"), (i as u64 + 1) * 37, vec![]))
                .collect(),
        );
        let tiles = layout(&tree, 3);
        for (i, a) in tiles.iter().enumerate() {
            for b in tiles.iter().skip(i + 1) {
                assert!(
                    a.rect.overlap(b.rect) < 1e-6,
                    "{} 与 {} 重叠了：{:?} / {:?}",
                    a.name,
                    b.name,
                    a.rect,
                    b.rect
                );
            }
        }
    }

    /// 块应当尽量方正：4:1 的画布 + 大小差不太多的条目，最差长宽比不该离谱。
    #[test]
    fn tiles_stay_reasonably_square() {
        let tree = root(
            (0..8)
                .map(|i| node(&format!("n{i}"), 100 - i as u64 * 5, vec![]))
                .collect(),
        );
        let tiles = layout(&tree, 3);
        let worst_ratio = tiles
            .iter()
            .map(|t| {
                let (w, h) = (t.rect.w.max(1e-6), t.rect.h.max(1e-6));
                (w / h).max(h / w)
            })
            .fold(1.0f32, f32::max);
        assert!(
            worst_ratio < 6.0,
            "最差长宽比 {worst_ratio} 太夸张（squarified 的意义就是别切出细长条）"
        );
    }

    /// 展开深度：到底的目录自己成块，没到底的目录让位给它的子项。
    #[test]
    fn directories_expand_until_max_depth() {
        let leaf = node("leaf.txt", 10, vec![]);
        let mid = node("mid", 0, vec![leaf]);
        let tree = root(vec![mid]);
        // depth 1：mid 展开，leaf 成块。
        let deep = layout(&tree, 2);
        assert_eq!(deep.len(), 1);
        assert_eq!(deep[0].name, "leaf.txt");
        assert_eq!(deep[0].depth, 1);
        // depth 0（max_depth=1）：mid 自己成块。
        let shallow = layout(&tree, 1);
        assert_eq!(shallow.len(), 1);
        assert_eq!(shallow[0].name, "mid");
        assert_eq!(shallow[0].depth, 0);
    }

    /// 全 0（一堆空文件）不画——否则会铺出一堆零尺寸的块。
    #[test]
    fn zero_sized_entries_are_omitted() {
        let tree = root(vec![node("empty", 0, vec![]), node("real", 5, vec![])]);
        let tiles = layout(&tree, 3);
        assert_eq!(tiles.len(), 1);
        assert_eq!(tiles[0].name, "real");
    }
}
