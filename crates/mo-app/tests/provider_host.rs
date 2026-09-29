//! P3 provider 宿主的验收测试（devlog §8）：「provider 卡死 / 崩了，UI 不受影响」
//! 的确定性测试 + 协议行为逐条钉死。
//!
//! 夹具是 `src/bin/p3_provider.rs`——一个会说协议的最小进程，行为由 argv 选
//! （ok / hang / crash / garbage / refuse）。`CARGO_BIN_EXE_p3_provider` 由 cargo
//! 在集成测试里自动提供，不依赖运行目录。

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use mo_app::provider::{base64_encode, build_classify_params, CallError, Manager, SpawnSpec};

mod common;

fn exe() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_p3_provider"))
}

fn spec_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mo-p3host-{}-{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn spec(tag: &str, mode: &str, call_ms: u64) -> SpawnSpec {
    let dir = spec_dir(tag);
    SpawnSpec {
        ext_id: format!("p3-{tag}"),
        argv: vec![exe().display().to_string(), mode.to_string()],
        dir: dir.clone(),
        methods: vec!["classify".into(), "preview".into()],
        capabilities: vec![],
        covered_exts: vec![".p3x".into()],
        log_path: dir.join("host.log"),
        startup_timeout: Duration::from_millis(2000),
        call_timeout: Duration::from_millis(call_ms),
    }
}

/// 用全新连接打开同一个缓存库（不经过 Manager 的懒持有，模拟「重启后」）。
fn mo_cache_or_fresh() -> mo_cache::ClassifyCache {
    mo_cache::ClassifyCache::open(&mo_cache::plugin_classify_file()).expect("重开缓存库")
}

fn classify_params(path: &Path) -> serde_json::Value {
    build_classify_params(path, 42, None)
}

/// 正常路径：握手 + classify（多余字段收下不用）+ preview 都答得上来。
#[test]
fn ok_mode_answers_classify_and_preview() {
    let host = mo_app::provider::Host::new(spec("ok", "ok", 2000));
    let p = Path::new("/tmp/movie.p3x");
    let result = host
        .call("classify", &classify_params(p))
        .expect("应当答上来");
    assert_eq!(result["label"], "P3测试种类");
    assert!(
        result.get("group").is_some(),
        "插件多答的字段收下不用，不该报错"
    );

    let pv = host
        .call(
            "preview",
            &serde_json::json!({"path": p.display().to_string()}),
        )
        .expect("preview 应当答上来");
    assert_eq!(pv["kind"], "markdown");
    assert_eq!(pv["text"], "# 来自插件");
}

/// `list` 方法（P4）：ok 模式答两行；宿主把 `source` **原样传给进程**（夹具把它
/// 回显进第一行的 name 里，传没传一眼可见）；解析侧收 name / path / subtitle，
/// icon 收下不用不报错。
#[test]
fn list_method_returns_rows_with_source_passthrough() {
    let host = mo_app::provider::Host::new(spec("list", "ok", 2000));
    let result = host
        .call("list", &mo_app::provider::list_params("recent"))
        .expect("list 应当答上来");
    let rows = mo_app::provider::list_rows_from_result(&result);
    assert_eq!(rows.len(), 2, "夹具答两行");
    assert_eq!(
        rows[0].name, "第一行 · recent",
        "宿主要把 source 原样传给进程"
    );
    assert_eq!(rows[0].subtitle.as_deref(), Some("副标题"));
    assert_eq!(rows[0].path.as_deref(), Some(Path::new("/tmp")));
    assert!(rows[1].path.is_none(), "没有路径的行就是「不动的一行」");
    assert!(
        !rows[0].is_dir && !rows[1].is_dir,
        "裸解析不 stat，is_dir 由 Manager::list 补"
    );
}

/// Manager 级的 list 端到端（P4）：清单（provider.methods=["list"] + lists 成对声明）
/// → 宿主 → 调用 → 解析 → **逐行 stat 补 is_dir**。带路径的行指向 /tmp（真目录），
/// is_dir 必须被标出来——双击导航判「进目录还是开文件」靠它。
#[test]
fn manager_list_source_end_to_end() {
    // ⚠️ 这条会真开 sqlite（`Manager::cache()` / `mo_cache_or_fresh()`），落点是
    // `MO_CACHE_DIR` 指的那个 `plugin-classify.sqlite`。不隔离就等于跟**别的测试进程**
    // 抢同一个库文件（进程各自的 env 互不影响，但磁盘上是同一份），表现为概率性的
    // 「缓存应能打开」失败——并行跑才红、单跑永远绿。
    common::isolated("p3list", || {
        let tmp = spec_dir("mgrlist");
        std::env::set_var("MO_CACHE_DIR", tmp.join("cache"));
        let config_json = tmp.join("config.json");
        std::fs::write(&config_json, "{}").unwrap();

        let ext_dir = tmp.join("extensions").join("p4a");
        std::fs::create_dir_all(&ext_dir).unwrap();
        std::fs::write(
            ext_dir.join("manifest.json"),
            format!(
                r#"{{"id":"p4a","name":"P4夹具","provider":{{"run":["{}","ok"],"methods":["list"]}},"lists":[{{"id":"recent","title":"最近文件"}}]}}"#,
                exe().display()
            ),
        )
        .unwrap();

        let manager = Manager::new();
        let rows = manager
            .list("p4a", "recent", &config_json)
            .expect("应当答上来");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name, "第一行 · recent");
        assert!(rows[0].is_dir, "宿主 stat 后要把 /tmp 标成目录");
        assert!(rows[1].path.is_none(), "无路径行保持「不动」");
        assert!(!rows[1].is_dir);
        let _ = std::fs::remove_dir_all(&tmp);
    })
}

/// 认不出的方法（尤其插件不该有的反向请求）：协议级 error，不炸宿主。
#[test]
fn unknown_method_comes_back_as_protocol_error() {
    let host = mo_app::provider::Host::new(spec("unknown", "ok", 2000));
    let err = host
        .call(
            "host.read_file",
            &serde_json::json!({"path": "/etc/passwd"}),
        )
        .expect_err("反向请求没有通道，必须是 error");
    assert!(matches!(err, CallError::Protocol(_)), "{err:?}");
    // 健康进程：拒答不进退避账，接着用还是好的。
    let p = Path::new("/tmp/movie.p3x");
    assert!(host.call("classify", &classify_params(p)).is_ok());
}

/// 「p3_provider hang」进程还活着吗（unix；kill 与否的唯一真可观察量——
/// 两次调用都 Timeout，进程级差别只有 pgrep 看得见）。
#[cfg(unix)]
fn hang_process_gone() -> bool {
    for _ in 0..40 {
        let out = std::process::Command::new("pgrep")
            .args(["-f", "p3_provider hang"])
            .output()
            .expect("pgrep");
        if out.status.success() && !out.stdout.is_empty() {
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }
        return true;
    }
    false
}

/// 卡死（devlog §8 的核心验收）：超时 kill，这一问按没回答处理；
/// 同一批里下一个调用会拿到一个**新的**进程、同样超时——UI 侧永远只是「慢一下」。
/// 进程真的死了用 pgrep 钉（行为上两次都是 Timeout，只有进程表分得出 kill 与否）。
#[test]
#[cfg(unix)]
fn hanging_provider_is_killed_on_deadline() {
    let host = mo_app::provider::Host::new(spec("hang", "hang", 300));
    let started = Instant::now();
    let err = host
        .call("classify", &classify_params(Path::new("/tmp/a.p3x")))
        .expect_err("卡死的进程必须超时");
    assert_eq!(err, CallError::Timeout);
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_millis(300) && elapsed < Duration::from_secs(5),
        "超时要按 deadline 兜住，不能死等：{elapsed:?}"
    );
    // 再问一次：旧进程已被 kill，起新进程、同样超时——不 panic、不泄漏、不复活僵尸。
    let err2 = host
        .call("classify", &classify_params(Path::new("/tmp/a.p3x")))
        .expect_err("第二次也该超时");
    assert_eq!(err2, CallError::Timeout);
    assert!(hang_process_gone(), "超时的进程必须被 kill，不许留孤儿");
}

/// 崩溃计数与退避：连续 3 次（起得来、一问就死）后进退避，之后调用**立刻**短路。
/// 进程起不来（argv 指向不存在的东西）走同一条账。
#[test]
fn repeated_crashes_trip_the_backoff() {
    let host = mo_app::provider::Host::new(spec("crash", "crash", 2000));
    let p = Path::new("/tmp/a.p3x");
    for i in 1..=3 {
        let err = host
            .call("classify", &classify_params(p))
            .expect_err("必崩");
        assert!(matches!(err, CallError::Crashed(_)), "第 {i} 次：{err:?}");
    }
    let started = Instant::now();
    let err = host.call("classify", &classify_params(p)).unwrap_err();
    assert_eq!(err, CallError::Backoff, "第 4 次应当被退避短路");
    assert!(
        started.elapsed() < Duration::from_millis(50),
        "短路必须立刻，不能又去起进程"
    );
}

/// 垃圾行：插件往 stdout 吐人话不炸宿主，跳过它继续等正经应答。
#[test]
fn garbage_lines_are_skipped() {
    let host = mo_app::provider::Host::new(spec("garbage", "garbage", 2000));
    let result = host
        .call("classify", &classify_params(Path::new("/tmp/a.p3x")))
        .expect("垃圾行后面的正经应答要被收下");
    assert_eq!(result["label"], "P3测试种类");
}

/// 起不来的进程（argv 指向不存在）也要进退避账，而不是每次调用白等。
#[test]
fn unspawnable_argv_counts_toward_backoff() {
    let mut bad = spec("nosuch", "ok", 500);
    bad.argv = vec!["/no/such/p3x-host-不存在".into()];
    let host = mo_app::provider::Host::new(bad);
    for i in 1..=3 {
        let err = host
            .call("classify", &classify_params(Path::new("/tmp/a.p3x")))
            .expect_err("起不来必须报错");
        assert!(matches!(err, CallError::Crashed(_)), "第 {i} 次：{err:?}");
    }
    assert!(host.disabled_until().is_some(), "三次起不来也该进退避");
}

/// Manager 端到端：清单 → 归属表 → 宿主 → 调用 → 落账（内存 + sqlite）→ 卸载清账。
///
/// 认 `MO_CACHE_DIR` / `MO_CONFIG_DIR` 把缓存与配置钉进临时目录——不钉的话这次测试
/// 会往开发机的真实缓存里写行。
#[test]
fn manager_end_to_end_classifies_stores_and_forgets() {
    // ⚠️ 这条会真开 sqlite（`Manager::cache()` / `mo_cache_or_fresh()`），落点是
    // `MO_CACHE_DIR` 指的那个 `plugin-classify.sqlite`。不隔离就等于跟**别的测试进程**
    // 抢同一个库文件（进程各自的 env 互不影响，但磁盘上是同一份），表现为概率性的
    // 「缓存应能打开」失败——并行跑才红、单跑永远绿。
    common::isolated("p3cache", || {
        let tmp = spec_dir("mgr");
        std::env::set_var("MO_CACHE_DIR", tmp.join("cache"));
        let config_json = tmp.join("config.json");
        std::fs::write(&config_json, "{}").unwrap();

        let ext_dir = tmp.join("extensions").join("p3a");
        std::fs::create_dir_all(&ext_dir).unwrap();
        // capabilities 带 read-contents：宿主应把文件头附进 classify 入参（capability
        // 的执行点在派发侧，夹具进程不管这个字段，但参数里必须看得见）。
        std::fs::write(
            ext_dir.join("manifest.json"),
            format!(
                r#"{{"id":"p3a","name":"P3夹具","provider":{{"run":["{}","ok"],"methods":["classify"]}},"capabilities":["read-contents"],"types":[{{"ext":[".p3x"],"label":"静态"}}]}}"#,
                exe().display()
            ),
        )
        .unwrap();
        let file = tmp.join("movie.p3x");
        std::fs::write(&file, b"P3X-FAKE-CONTENT").unwrap();

        let manager = Manager::new();
        assert_eq!(
            manager.owner_of(&file, &config_json).as_deref(),
            Some("p3a"),
            ".p3x 应归 p3a"
        );
        assert!(
            manager
                .owner_of(&tmp.join("other.txt"), &config_json)
                .is_none(),
            "没归属的扩展名不该有 owner"
        );

        let host = manager.host("p3a", &config_json).expect("应有宿主");
        // capability 执行点：授了 read-contents，入参里带 head_b64。
        let md = std::fs::metadata(&file).unwrap();
        let head = mo_app::provider::read_head(&file);
        let params = build_classify_params(&file, md.len(), head.as_deref());
        assert_eq!(
            params["head_b64"],
            serde_json::json!(base64_encode(b"P3X-FAKE-CONTENT")),
            "授了 read-contents 就要附文件头"
        );
        let result = host.call("classify", &params).expect("应当答上来");
        let label = mo_app::provider::classify_label_of_result(&result).expect("应有标签");

        // 真实流程里 cache() 先于落账被打开（classify_batch 的第一步）；这里同序。
        let _cache = manager.cache().expect("缓存应能打开");
        let mtime: i64 = md
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        manager.store_labels(&[("p3a".into(), file.clone(), mtime, md.len() as i64, label)]);
        assert_eq!(
            manager.classify_label_of(&file).as_deref(),
            Some("P3测试种类")
        );

        // sqlite 落盘：换一个连接（模拟重启）从缓存库里还能捞回同一行。
        let fresh = mo_cache_or_fresh();
        let row = fresh.get("p3a", &file).expect("查缓存").expect("应命中");
        assert_eq!(row.label, "P3测试种类");
        assert_eq!(row.size, md.len() as i64);

        // 卸载清账：宿主、内存表、sqlite 行一起消失。
        manager.forget_ext("p3a");
        assert!(manager.classify_label_of(&file).is_none(), "内存表要清");
        assert!(
            fresh.get("p3a", &file).expect("查缓存").is_none(),
            "sqlite 行也要清"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    })
}
