//! 同目录刷新不该丢掉**已解码**的缩略图（devlog/windows-port.md §44）。
//!
//! 用户报的症状：「新建一个文本文件的时候，文件列表的图标会闪一下——一张图片
//! 先变成默认图标，再变回它原本的缩略图。」机制在模型层：新建文件之后走的是
//! 一次整目录重读（`AppState::load_path`），新 `Entry` 一律
//! `ThumbnailState::Idle`，而位图是**异步**回填的（调度器排队 → 信号量 →
//! 后台解码 → `set_thumbnail`，中间还夹着 120ms 的广播合并）——于是可见的
//! 图片行实实在在画了一两帧 fallback。修法是换上新快照前按 `FileId` 把旧目录
//! 里的 `Loaded` 位图搬过来（`thumbnail::carry_thumbnails`）。
//!
//! 守卫落在 `mo-app` 而不是 UI：被改的就是这一层的模型搬运，UI 只是它的投影；
//! 且 headless 里真解码一张图要磁盘缓存 + 图像源在场，测不到点上。

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use mo_app::AppState;
use mo_core::{Bitmap, ThumbnailState};

mod common;

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Runtime::new().unwrap()
}

fn tree(tag: &str, files: &[&str]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mo-thumbcarry-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    for name in files {
        fs::write(dir.join(name), b"x").unwrap();
    }
    dir
}

fn trash_root(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("mo-thumbcarry-trash-{tag}-{}", std::process::id()))
}

/// 一张可辨识的位图：`Bitmap::id()` 每次构造都不同，正好用来验「搬过来的是
/// **同一张**，不是又排了一次队」。
fn bitmap() -> Arc<Bitmap> {
    Arc::new(Bitmap::from_rgba(2, 2, vec![0u8; 2 * 2 * 4]).expect("2x2 位图"))
}

async fn thumb(app: &AppState, name: &str) -> ThumbnailState {
    app.current_entries()
        .await
        .into_iter()
        .find(|e| e.name == name)
        .unwrap_or_else(|| panic!("{name} 不在列表里"))
        .thumbnail
}

/// 本轮的主 case：图片已解码 → 新建一个文本文件 → 同目录刷新 → 那一格还是
/// **同一张位图**，不落回 `Idle`（落回就会被 UI 重新排队，闪一下）。
#[test]
fn a_refresh_carries_the_decoded_thumbnail() {
    let dir = tree("carry", &["pic.png"]);
    let app = common::isolated("carry", || AppState::with_trash(trash_root("carry")));

    runtime().block_on(async {
        app.open_local(&dir).await.expect("打开本地目录");
        let id = app
            .current_entries()
            .await
            .iter()
            .find(|e| e.name == "pic.png")
            .expect("pic.png 在列表里")
            .id;
        let bm = bitmap();
        let bm_id = bm.id();
        app.set_thumbnail(id, ThumbnailState::Loaded(bm)).await;
        assert!(
            matches!(thumb(&app, "pic.png").await, ThumbnailState::Loaded(_)),
            "前置条件：位图得先落在条目上（否则下面的断言只是「从来没有过」）"
        );

        // 用户的那一下：目录里多了一个文本文件。
        fs::write(dir.join("new.txt"), b"x").unwrap();
        app.refresh().await.expect("刷新目录");

        match thumb(&app, "pic.png").await {
            ThumbnailState::Loaded(bm) => assert_eq!(
                bm.id(),
                bm_id,
                "刷新后不是原先那张位图：说明没搬过来，UI 会重画 fallback"
            ),
            other => panic!(
                "同目录刷新把已解码的缩略图打回 {other:?}——图片行会先画默认图标，\
                 再等异步回填，这就是用户看到的「闪一下」"
            ),
        }
    });
    let _ = fs::remove_dir_all(&dir);
}

/// 反面对照：这次刷新里**新出现**的文件必须还是 `Idle`——搬运只搬旧目录里已有
/// 位图的那几条，不能凭空给新行造一张图（新行该走正常排队）。
///
/// 新文件故意取 `a_late.png`：按名的默认排序下它排在 `pic.png` **前面**，于是
/// 「按下标把旧目录那排状态抄过来」这种偷懒写法会同时错两处（a_late 被塞了图、
/// pic 丢了图），而按 `FileId` 对的写法两处都稳。
#[test]
fn a_new_entry_stays_idle_while_the_old_one_keeps_its_bitmap() {
    let dir = tree("newer", &["pic.png"]);
    let app = common::isolated("newer", || AppState::with_trash(trash_root("newer")));

    runtime().block_on(async {
        app.open_local(&dir).await.expect("打开本地目录");
        let id = app
            .current_entries()
            .await
            .iter()
            .find(|e| e.name == "pic.png")
            .expect("pic.png 在列表里")
            .id;
        app.set_thumbnail(id, ThumbnailState::Loaded(bitmap()))
            .await;

        fs::write(dir.join("a_late.png"), b"x").unwrap();
        app.refresh().await.expect("刷新目录");
        let rs = app.current_entries().await;
        assert_eq!(
            rs.first().map(|e| e.name.as_str()),
            Some("a_late.png"),
            "前置条件没成立：新文件没排在 pic.png 之前，这条对照就退化了"
        );

        assert!(
            matches!(thumb(&app, "pic.png").await, ThumbnailState::Loaded(_)),
            "老条目的位图该保住"
        );
        assert!(
            matches!(thumb(&app, "a_late.png").await, ThumbnailState::Idle),
            "新出现的图片条目被凭空填了状态——搬运过头了"
        );
    });
    let _ = fs::remove_dir_all(&dir);
}

/// 只搬 `Loaded`：一次偶发的解码失败（`Failed`）不该被钉死在这一行上——
/// 刷新后回到 `Idle`，下一轮排队还有救回来的机会。
#[test]
fn a_failed_thumbnail_is_not_carried() {
    let dir = tree("failed", &["pic.png"]);
    let app = common::isolated("failed", || AppState::with_trash(trash_root("failed")));

    runtime().block_on(async {
        app.open_local(&dir).await.expect("打开本地目录");
        let id = app
            .current_entries()
            .await
            .iter()
            .find(|e| e.name == "pic.png")
            .expect("pic.png 在列表里")
            .id;
        app.set_thumbnail(id, ThumbnailState::Failed).await;

        fs::write(dir.join("other.txt"), b"x").unwrap();
        app.refresh().await.expect("刷新目录");

        assert!(
            matches!(thumb(&app, "pic.png").await, ThumbnailState::Idle),
            "失败状态被搬过来了：这一行再也试不回缩略图"
        );
    });
    let _ = fs::remove_dir_all(&dir);
}
