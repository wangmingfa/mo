//! 集成测试的进程卫生：把**配置目录**与**缓存目录**钉进临时目录。
//!
//! `AppState` 一构造就接上 `<缓存>/mo/search.sqlite` 的全局索引，并读 `<配置>/config.json`
//! （视图模式、侧边栏开关这些偏好）。不隔离就是两件事同时发生：往开发者机器上的真索引里
//! 堆一批临时测试目录（用户回头按搜索键找文件名，会搜出一批早已被删掉的 `mo-trash-*`），
//! 以及同一份代码在不同机器上因为读到不同偏好而结论不同。
//!
//! ⚠️ 这两个是**进程级**环境变量，而同二进制的用例默认多线程并行：A 刚把变量指到自己的
//! 目录，B 又把它改走，之后谁构造 `AppState` 打开的就是别人的库——而且这类竞态单跑永远
//! 复现不了（单线程 = 没有并行）。所以「改变量 + 当场构造」必须一起发生在 [`env_lock`]
//! 之下，这就是 [`isolated`] 存在的理由：把两步绑在一起，用例那边漏不了。
//!
//! 每个 `tests/*.rs` 都是一个**单独的可执行文件**，各自带一份本模块；目录名里再带上进程号，
//! 所以不同文件撞用同一个 `tag` 也不会互相踩。

#![allow(dead_code)] // 各文件用到的子集不同，本文件里没用上的不算死代码。

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};

static ENV_LOCK: Mutex<()> = Mutex::new(());

/// 串行化「动环境变量的那几行」——注意锁只在构造期间持有，不覆盖整个用例。
fn env_lock() -> MutexGuard<'static, ()> {
    // 中毒的锁说明某个持锁用例 panic 过。直接把锁拿回来：前一个用例的失败不该
    // 把后面每一个都拖成「获取锁失败」那种跟本案无关的红。
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// `tag` 那一档的隔离目录。配置与缓存合成同一个根：少一个变量就少一处漏钉。
fn store(tag: &str) -> PathBuf {
    // 进程级一次：顺手清掉**别的**测试进程留下的过期目录（含 `mo-trash-*`）。
    // 规则与守卫见 `mo_fs::sweep_stale_temp_dirs`；本进程自己的目录以 `-{pid}`
    // 结尾，清扫自己不会碰到。
    static SWEPT: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    SWEPT.get_or_init(|| {
        mo_fs::sweep_stale_temp_dirs(
            &["mo-app-store-", "mo-trash-"],
            std::time::Duration::from_secs(24 * 60 * 60),
        );
    });
    let dir = std::env::temp_dir().join(format!("mo-app-store-{tag}-{}", std::process::id()));
    // 先清一遍：同名目录可能是上一次跑（同 pid 复用）留下的索引，读脏了会看错。
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("建隔离目录");
    dir
}

/// 在按 `tag` 隔离的用户目录里构造 `build`（通常就是一个 `AppState`）。
///
/// ```ignore
/// let app = common::isolated("del", || AppState::with_trash(trash.clone()));
/// ```
///
/// ⚠️ 每调一次就把那个目录**清空重建**一遍，所以「重开应用后索引还在不在」这类
/// 要在同一个库里跑两趟的用例，得把两次构造（以及中间那段等待）都放进**同一次**
/// [`isolated`] 的闭包里，而不是把同一个 `tag` 传两次——第二次会把上一轮写下的
/// 索引抹成一个空库。（POSIX 上 `unlink` 对已打开的文件照样生效；Windows 上
/// 句柄占着、恰好抹不动，于是换个平台就红。）
///
/// 锁在 `build` 返回后就放开：`AppState` 当场已经把库句柄指到那个临时文件，之后
/// 别的用例把环境变量改走也影响不到已经建好的这一个。但**每次调用都现读环境变量**
/// 的那类接口（`preview_pdf_page` 读 `MO_CACHE_DIR` 落渲染产物）不在这个保护里，
/// 这种用例就把整段放进一次 `isolated`。
pub fn isolated<T>(tag: &str, build: impl FnOnce() -> T) -> T {
    let _env = env_lock();
    let dir = store(tag);
    std::env::set_var("MO_CONFIG_DIR", &dir);
    std::env::set_var("MO_CACHE_DIR", &dir);
    build()
}
