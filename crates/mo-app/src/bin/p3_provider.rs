//! * `ok`（缺省）： initialize / classify（答固定标签）/ preview（答 markdown）/
//!   list（答两行：一行带路径 + 副标题、一行只有名字）都正常。
//! * `hang`：握手正常，`classify` 永不应答（测超时 kill）。
//! * `crash`：握手正常，第一次 `classify` 直接退出（测崩溃计数）。
//! * `garbage`：答 `classify` 前先往 stdout 吐一行不成帧的人话（测「跳过垃圾行不炸」）。
//! * `refuse`：`classify` 回协议级 error（测「健康地拒答不计失败」）。

use std::io::{stdin, BufRead};

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "ok".into());
    for line in stdin().lock().lines() {
        let Ok(line) = line else { break };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        let id = v["id"].as_u64().unwrap_or(0);
        match v["method"].as_str().unwrap_or("") {
            "initialize" => println!(
                r#"{{"id":{id},"result":{{"name":"p3-provider","version":"1.0","methods":["classify","preview","list"]}}}}"#
            ),
            "shutdown" => {
                println!(r#"{{"id":{id},"result":null}}"#);
                break;
            }
            "classify" => match mode.as_str() {
                // 永不应答：宿主会按超时 kill 我们。
                "hang" => std::thread::sleep(std::time::Duration::from_secs(3600)),
                // 进程级崩溃：退出码非 0，stdout 直接关闭。
                "crash" => std::process::exit(9),
                // 先吐一行不成帧的，再正常答——宿主要跳过前者、收下后者。
                "garbage" => {
                    println!("this is not json at all");
                    println!(
                        r#"{{"id":{id},"result":{{"label":"P3测试种类","group":"ignored"}}}}"#
                    );
                }
                // 健康地拒答：协议级 error，不计失败。
                "refuse" => println!(r#"{{"id":{id},"error":"我不想答这一问"}}"#),
                _ => println!(
                    r#"{{"id":{id},"result":{{"label":"P3测试种类","group":"ignored","icon_key":"x"}}}}"#
                ),
            },
            "preview" => {
                println!(r##"{{"id":{id},"result":{{"kind":"markdown","text":"# 来自插件"}}}}"##)
            }
            // list（P4）：source 原样回显进第一行的 id 里（证明宿主真的传了），
            // 两行分别覆盖「带路径 + 副标题」与「只有名字」两种形态。
            "list" => {
                let source = v["params"]["source"].as_str().unwrap_or("");
                println!(
                    r##"{{"id":{id},"result":{{"rows":[{{"id":"r1","name":"第一行 · {source}","path":"/tmp","subtitle":"副标题","icon":"ignored"}},{{"id":"r2","name":"第二行"}}]}}}}"##
                )
            }
            // 认不出的方法（含插件不该有的反向请求）：走 error 臂。
            other => println!(r#"{{"id":{id},"error":"认不出的方法 {other}"}}"#),
        }
    }
}
