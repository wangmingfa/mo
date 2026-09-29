//! 临时探针：把给定路径写进**真实的**系统剪贴板，供真机验证「Mo 里复制 →
//! 访达 / 其它应用粘得出」。
//!
//! ⚠️ 会覆盖你机器当前的剪贴板内容（无法找回）。
//!
//! 用法：`cargo run -p mo-platform --example clipboard_probe -- /path/a /path/b ...`
//! 写完后可用下面这段 JXA 读回验证：
//!
//! ```sh
//! osascript -l JavaScript -e '
//!   ObjC.import("AppKit");
//!   var urls = $.NSPasteboard.generalPasteboard
//!       .readObjectsForClassesForOptions($.NSArray.arrayWithObject($.NSURL), null);
//!   ObjC.unwrap(urls).map(String.join("", $()))'
//! ```

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let paths: Vec<std::path::PathBuf> = args.iter().map(std::path::PathBuf::from).collect();
    if paths.is_empty() {
        eprintln!("用法：clipboard_probe <路径>...");
        std::process::exit(2);
    }
    match mo_platform::write_file_clipboard(&paths, false) {
        Ok(()) => println!("OK：已把 {} 条路径写进系统剪贴板", paths.len()),
        Err(e) => {
            eprintln!("失败：{e}");
            std::process::exit(1);
        }
    }
    // NSPasteboard 的 `writeObjects` 是**惰性**的：数据提供者是本进程里的 NSURL
    // 对象，别的应用来读时剪贴板服务进程会回头找写方要数据——写完立刻退出的
    // 进程什么都留不下（真机验证时踩过：JXA 跨进程读回是空的）。
    // Mo 本体是长驻应用没有这个问题；探针得等一拍。
    eprintln!("保持存活 3 秒，供别的进程来读…");
    std::thread::sleep(std::time::Duration::from_secs(3));
}
