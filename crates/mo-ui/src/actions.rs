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
//! 而那份数据现在来自**声明本身**：命令 / 工作流上的 `menu` 字段（见
//! [`mo_app::MenuSlot`]），所以「让一条扩展命令出现在右键菜单里」不再需要改任何
//! 调用点——写清单的人回答一句「它出现在哪儿」就够了。
//!
//! ## 索引语义（⚠️ 别当成稳定的身份）
//!
//! [`ActionKind::User`] / [`ActionKind::Workflow`] 存的是**建这张表时那两份 vec** 里的
//! 下标。谁建的表谁负责把载荷一起带上（[`Contributions::payload`]），查不到就当这条不
//! 存在——`RootView` 上那份镜像随时会被命令面板重取，拿它当下标参照就会点 A 跑出 B。
//! 命令面板那条路仍是下标（打开时重取、按下 Enter 立刻执行，中间没有别人），
//! 与既有的 `CommandId::User(usize)` 同语义。

/// 一条动作可以出现的界面位置。
///
/// 类型本体在 `mo-config`（[`mo_app::MenuSlot`]）——那里是它**被写下来**的地方：
/// 配置与扩展清单里的 `menu: ["palette", "context:file"]` 就译成这个枚举。UI 这边
/// 只是它**被读出来**的一端，再定义一份枚举就得在两个 crate 之间来回转换，而这两份
/// 定义除了同步之外没有任何用途。
pub(crate) type Slot = mo_app::MenuSlot;

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
/// 每条落在哪些界面，由那一条**自己的** `menu` 声明决定（[`mo_app::MenuSlot::defaults`]
/// = 没写就只进命令面板，与 P2 之前逐条一致）。这里**不**再看扩展名：`users` 由
/// `mo_app::user_commands()` 交上来时已经按清单的 `when_ext` 过滤过，两处都判就会出现
/// 「面板看得到、菜单看不到」这类对不上号的差异。
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
            slots: u.slots(),
        });
    }
    for (i, w) in workflows.iter().enumerate() {
        out.push(ActionSpec {
            title: w.name.clone(),
            category: "工作流".to_string(),
            kind: ActionKind::Workflow(i),
            slots: w.slots(),
        });
    }
    out
}

/// 命中某个槽位的那些（保持注册表里的顺序）。
pub(crate) fn for_slot(specs: &[ActionSpec], slot: Slot) -> Vec<&ActionSpec> {
    specs.iter().filter(|s| s.goes_to(slot)).collect()
}

/// 一张注册表**连着它对应的那两份载荷**。
///
/// ⚠️ 为什么要连载荷一起交出去：[`ActionKind::User`] 里的下标指的是**这一批** `users`
/// 的位置。如果只交注册表、点击时再去 `RootView::user_commands` 那份镜像里查，就留着
/// 一个空档：菜单开着的时候镜像被人重取一次（再开一次命令面板就会），下标飘了，点 A
/// 跑出 B。绑成一份数据之后，交出去的那一刻起谁也无法让它俩对不上——
/// 「载荷带身份而不是下标」这条纪律（见 §4.2）在这里的兑现形式就是这个结构。
#[derive(Clone, Debug, Default)]
pub(crate) struct Contributions {
    pub specs: Vec<ActionSpec>,
    pub commands: Vec<mo_app::UserCommand>,
    pub workflows: Vec<mo_app::Workflow>,
}

impl Contributions {
    /// 由「这一批用户命令 + 这一批工作流」建一份。两边的下标从此锁死在这份数据上。
    pub(crate) fn of(users: &[mo_app::UserCommand], workflows: &[mo_app::Workflow]) -> Self {
        Self {
            specs: contributed(users, workflows),
            commands: users.to_vec(),
            workflows: workflows.to_vec(),
        }
    }

    /// 这条动作的执行载荷。`None` = 这份数据里没这一条（清单在菜单打开后被人删了，
    /// 或者下标本来就不该出现在这里）——调用方什么都不做，绝不按位置猜一条顶上。
    pub(crate) fn payload(&self, kind: ActionKind) -> Option<Payload> {
        match kind {
            ActionKind::User(i) => self.commands.get(i).cloned().map(Payload::User),
            ActionKind::Workflow(i) => self.workflows.get(i).cloned().map(Payload::Workflow),
        }
    }
}

/// 一条贡献动作的实际可执行内容。
pub(crate) enum Payload {
    User(mo_app::UserCommand),
    Workflow(mo_app::Workflow),
}

#[cfg(test)]
mod tests {
    // ⚠️ 不 `use super::*` 之外还要留意：本 crate 顶层有 `use gpui_kit::*`，
    // 这里没有，但内置 `#[test]` 与 gpui 的 test 宏撞车的事发生过（见 context_menu）。
    use super::{contributed, for_slot, ActionKind, Contributions, Payload, Slot};

    fn user(name: &str, category: &str) -> mo_app::UserCommand {
        user_in(name, category, &[])
    }

    /// `menu` 收的是**清单里写下来的那些字符串**（不是枚举）——这一层要验的正是
    /// 「一句声明走到了哪些界面上」，直接构造枚举就把它要测的东西绕过去了。
    fn user_in(name: &str, category: &str, menu: &[&str]) -> mo_app::UserCommand {
        mo_app::UserCommand {
            name: name.to_string(),
            category: category.to_string(),
            shell: "wc -l {file}".to_string(),
            source: None,
            menu: menu.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn workflow(name: &str) -> mo_app::Workflow {
        workflow_in(name, &[])
    }

    fn workflow_in(name: &str, menu: &[&str]) -> mo_app::Workflow {
        mo_app::Workflow {
            name: name.to_string(),
            steps: vec!["pwd".to_string()],
            source: None,
            menu: menu.iter().map(|s| s.to_string()).collect(),
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

    /// **没写** `menu` 的命令仍只投面板——P2 之前所有已有配置与清单都没有这个字段，
    /// 这条断言就是「加字段不改行为」的证明。反向验证的靶子：把 `contributed` 里的
    /// slots 换成硬编码 `vec![Slot::Palette]`，下一条必红。
    #[test]
    fn no_declaration_stays_palette_only() {
        let specs = contributed(&[user("统计", "")], &[workflow("打包")]);
        assert!(specs.iter().all(|s| s.slots == vec![Slot::Palette]));
        assert_eq!(for_slot(&specs, Slot::ContextFile).len(), 0);
        assert_eq!(for_slot(&specs, Slot::ContextBlank).len(), 0);
        assert_eq!(for_slot(&specs, Slot::Palette).len(), 2);
    }

    /// 写了 `menu` 就**只**去写的那些界面：`context:file` 让扩展命令第一次出现在
    /// 右键菜单里，而它从此不再出现在命令面板（显式列表 = 精确投递，不是追加）。
    /// 连字符写法与冒号写法等价（手改配置时更容易写对）。
    #[test]
    fn declared_slots_reach_exactly_the_listed_surfaces() {
        let specs = contributed(
            &[
                user_in("统计字数", "", &["context:file"]),
                user_in("归档到当前目录", "", &["context-blank", "palette"]),
                user_in(
                    "转换格式",
                    "媒体",
                    &["palette", "context:file", "context:blank"],
                ),
            ],
            &[workflow_in("打包", &["context:file"])],
        );
        let hit = |slot| {
            for_slot(&specs, slot)
                .into_iter()
                .map(|s| s.title.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(hit(Slot::Palette), ["归档到当前目录", "转换格式"]);
        assert_eq!(hit(Slot::ContextFile), ["统计字数", "转换格式", "打包"]);
        assert_eq!(hit(Slot::ContextBlank), ["归档到当前目录", "转换格式"]);
        // 一条声明了三个界面的命令，在三处都必须是同一条（下标一致才谈得上执行）。
        let all_three: Vec<usize> = specs
            .iter()
            .filter(|s| s.slots.len() == 3)
            .map(|s| match s.kind {
                ActionKind::User(i) => i,
                ActionKind::Workflow(i) => 1000 + i,
            })
            .collect();
        assert_eq!(all_three, vec![2]);
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

    /// 下标必须指回**建表时那两份 vec**：注册表交出去之后，原来那份切片被人丢了、
    /// 或者镜像重排了，都不该影响这一份数据里下标对应的载荷。
    #[test]
    fn indices_resolve_against_the_table_not_some_later_vec() {
        let users = [user("统计", ""), user("转换", "媒体")];
        let wfs = [workflow("打包")];
        let t = Contributions::of(&users, &wfs);

        // 两条用户命令 + 一条工作流，下标各自成段。
        let ActionKind::User(a) = t.specs[0].kind else {
            unreachable!()
        };
        let ActionKind::User(b) = t.specs[1].kind else {
            unreachable!()
        };
        let ActionKind::Workflow(w) = t.specs[2].kind else {
            unreachable!()
        };
        assert_eq!(a, 0);
        assert_eq!(b, 1);
        match t.payload(ActionKind::User(a)).unwrap() {
            Payload::User(c) => assert_eq!(c.name, "统计"),
            Payload::Workflow(_) => panic!("串了类型"),
        }
        match t.payload(ActionKind::User(b)).unwrap() {
            Payload::User(c) => assert_eq!(c.name, "转换"),
            Payload::Workflow(_) => panic!("串了类型"),
        }
        // 工作流的下标是从 0 数起的第二段，别与用户命令混到同一个 vec 里。
        match t.payload(ActionKind::Workflow(w)).unwrap() {
            Payload::Workflow(x) => assert_eq!(x.name, "打包"),
            Payload::User(_) => panic!("串了类型"),
        }
        // 越界 = 没有这条，而不是拿别的位置顶上（否则错一个下标就静默执行别人的命令）。
        assert!(t.payload(ActionKind::User(2)).is_none());
        assert!(t.payload(ActionKind::Workflow(0)).is_some());
        assert!(t.payload(ActionKind::Workflow(1)).is_none());
        // 丢掉原切片后仍自足（`of` 收的是引用）。
        drop(users);
        drop(wfs);
        assert_eq!(t.specs.len(), 3);
    }
}
