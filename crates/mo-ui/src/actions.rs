//! 动作注册表：**外面贡献进来的动作**（用户自定义命令 / 扩展命令 / 工作流）
//! 出现在哪些槽位、点了跑什么，只在这一处回答。
//!
//! ## 为什么内置命令不搬进来
//!
//! 内置那 ~90 条各自有 `CommandId` 变体与 `run_command` 分支，**编译期就穷尽**——
//! 少一个分支编译不过。把它们摊成一张数据表，等于把编译期保证换成运行期查表，
//! 换不到任何好处。这里收的是另一半：数量不定、来源可变（配置文件、扩展清单，
//! 以后还有插件）的那批。它们的共同点是「宿主事先不知道有几条」。
//!
//! ## 这一层解决的实际问题
//!
//! 之前「一条扩展命令能出现在哪儿」是**写死在调用点上**的：命令面板尾部两个 for
//! 循环（`commands_in`）是它唯一的路，右键菜单根本不知道有这批命令。想加一个
//! 「右键菜单里的扩展命令」就得再在 `items()` 里手写一个循环——两处各写一份，
//! 于是必然出现「面板里有、菜单里没有」这种没人解释得了的差异（与
//! [devlog/engine-testing.md §7](engine-testing) 那三份扩展名表同一个病）。
//! 收成一张表之后，槽位是**数据**（[`Slot`]），两个界面各自查同一张表。
//!
//! ## 索引语义（⚠️ 别当成稳定的身份）
//!
//! [`ActionKind::User`] / [`ActionKind::Workflow`] 存的是 `RootView::user_commands`
//! / `workflows` 里的**下标**，不是重建后的位置：点击时按当前那份 vec 取。
//! 也就是说「面板/菜单打开」到「执行」之间如果用户改了配置导致清单重排，
//! 执行的可能是另一条。这与既有的 `CommandId::User(usize)` 完全同语义，
//! 窗口只有一瞬，本层不额外加固；真要加固，做法是改成按 `name + source` 反查。

/// 一条动作可以出现的界面位置。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Slot {
    /// 命令面板（⌘K / Ctrl+Shift+P 那一个）。
    Palette,
    /// 右键菜单：对着一个条目（文件或目录都算，目录级差异由条目自己决定时再说）。
    ContextFile,
    /// 右键菜单：空白处（动作对象是当前目录）。
    ContextBlank,
}

/// 一条动作**干什么**。
///
/// 刻意不含 `Shell` 字符串之类的执行细节——那些留在 `mo_app::UserCommand` 里，
/// 执行路径（占位符守卫、输出回显）与今天完全同源，本层只做「投递」。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ActionKind {
    /// 用户自定义命令 / 扩展命令（下标指向 `RootView::user_commands`）。
    User(usize),
    /// 自动化工作流（下标指向 `RootView::workflows`）。
    Workflow(usize),
}

/// 一条贡献进来的动作。
#[derive(Debug, Clone)]
pub(crate) struct ActionSpec {
    pub title: String,
    /// 命令面板里的分组名（菜单不用这一列）。
    pub category: String,
    pub kind: ActionKind,
    /// 出现在哪些槽位。**空 = 哪儿都不出现**（宁可少显示，不让它冒出来）。
    pub slots: Vec<Slot>,
}

impl ActionSpec {
    pub(crate) fn goes_to(&self, slot: Slot) -> bool {
        self.slots.contains(&slot)
    }
}

/// 把「用户命令 + 工作流」摊成一张注册表。
///
/// 今天两类都只投**命令面板**一个槽位——这与收口前的行为逐条一致（收口的目的就是
/// 让「加一个槽位」变成改数据）。`users` 由 `mo_app::user_commands()` 交上来时
/// 已经按扩展清单的 `when_ext` 过滤过，所以这里**不再**判扩展名：两处都判就会
/// 出现「面板看得到、菜单看不到」这类对不上号的差异。
pub(crate) fn contributed(
    users: &[mo_app::UserCommand],
    workflows: &[mo_app::Workflow],
) -> Vec<ActionSpec> {
    let mut out = Vec::with_capacity(users.len() + workflows.len());
    for (i, u) in users.iter().enumerate() {
        out.push(ActionSpec {
            title: u.name.clone(),
            category: if u.category.trim().is_empty() {
                "自定义".to_string()
            } else {
                u.category.clone()
            },
            kind: ActionKind::User(i),
            slots: vec![Slot::Palette],
        });
    }
    for (i, w) in workflows.iter().enumerate() {
        out.push(ActionSpec {
            title: w.name.clone(),
            category: "工作流".to_string(),
            kind: ActionKind::Workflow(i),
            slots: vec![Slot::Palette],
        });
    }
    out
}

/// 命中某个槽位的那些（保持注册表里的顺序）。
pub(crate) fn for_slot(specs: &[ActionSpec], slot: Slot) -> Vec<&ActionSpec> {
    specs.iter().filter(|s| s.goes_to(slot)).collect()
}

#[cfg(test)]
mod tests {
    // ⚠️ 不 `use super::*` 之外还要留意：本 crate 顶层有 `use gpui_kit::*`，
    // 这里没有，但内置 `#[test]` 与 gpui 的 test 宏撞车的事发生过（见 context_menu）。
    use super::{contributed, for_slot, ActionKind, Slot};

    fn user(name: &str, category: &str) -> mo_app::UserCommand {
        mo_app::UserCommand {
            name: name.to_string(),
            category: category.to_string(),
            shell: "wc -l {file}".to_string(),
            source: None,
        }
    }

    fn workflow(name: &str) -> mo_app::Workflow {
        mo_app::Workflow {
            name: name.to_string(),
            steps: vec!["pwd".to_string()],
            source: None,
        }
    }

    /// 顺序与分类必须与收口前 `commands_in` 尾部那两个 for 循环一致：
    /// 用户命令在前、工作流在后；空分类落「自定义」，工作流恒「工作流」。
    #[test]
    fn registry_preserves_order_and_categories() {
        let specs = contributed(
            &[user("统计", ""), user("转换", "媒体")],
            &[workflow("打包")],
        );
        let got: Vec<(String, String, ActionKind)> = specs
            .iter()
            .map(|s| (s.title.clone(), s.category.clone(), s.kind))
            .collect();
        assert_eq!(
            got,
            vec![
                (
                    "统计".to_string(),
                    "自定义".to_string(),
                    ActionKind::User(0)
                ),
                ("转换".to_string(), "媒体".to_string(), ActionKind::User(1)),
                (
                    "打包".to_string(),
                    "工作流".to_string(),
                    ActionKind::Workflow(0)
                ),
            ]
        );
    }

    /// 今天两类**只**投面板——这条是「收口不改行为」的正面断言，
    /// 也是反向验证的靶子：把 `contributed` 里的 slots 加上 ContextFile，这条必红。
    #[test]
    fn everything_is_palette_only_for_now() {
        let specs = contributed(&[user("统计", "")], &[workflow("打包")]);
        assert!(specs.iter().all(|s| s.slots == vec![Slot::Palette]));
        assert_eq!(for_slot(&specs, Slot::ContextFile).len(), 0);
        assert_eq!(for_slot(&specs, Slot::ContextBlank).len(), 0);
        assert_eq!(for_slot(&specs, Slot::Palette).len(), 2);
    }

    /// 空 slots = 哪儿都不出现（默认拒绝，而不是默认全开）。
    #[test]
    fn empty_slots_appears_nowhere() {
        let mut specs = contributed(&[user("统计", "")], &[]);
        specs[0].slots.clear();
        for slot in [Slot::Palette, Slot::ContextFile, Slot::ContextBlank] {
            assert!(for_slot(&specs, slot).is_empty(), "{slot:?} 不该出现");
        }
    }
}
