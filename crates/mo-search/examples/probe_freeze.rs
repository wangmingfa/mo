//! 一次性探针：在真实索引库的副本上量主线程相关操作的墙钟（用完即删）。
use rusqlite::{params, Connection};
use std::time::Instant;

fn main() -> rusqlite::Result<()> {
    let path = std::env::args().nth(1).expect("usage: probe <sqlite copy>");
    let conn = Connection::open(&path)?;
    conn.busy_timeout(std::time::Duration::from_secs(2))?;
    conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;")?;

    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
        .unwrap();
    println!("rows = {n}");

    // 取样几条真实路径，保证「命中」的测例测的是真前缀。
    let mut stmt = conn.prepare("SELECT path FROM files WHERE is_dir = 1 LIMIT 3")?;
    let dirs: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .filter_map(|x| x.ok())
        .collect();
    println!("sample dirs = {dirs:?}");

    for round in 0..3 {
        let t = Instant::now();
        let c: i64 = conn
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
            .unwrap();
        println!("round {round}: COUNT(*) = {c} in {:?}", t.elapsed());
    }

    // 现在的 remove_under：LIKE 前缀（默认大小写不敏感，用不上 idx_path）
    let miss = dirs
        .first()
        .map(|d| format!("{d}.__probe_miss__"))
        .unwrap_or_else(|| "Q:/nope/x".into());
    for round in 0..3 {
        let t = Instant::now();
        let changed = conn.execute(
            "DELETE FROM files WHERE path LIKE ?1",
            params![format!("{miss}/%")],
        )?;
        println!(
            "round {round}: LIKE 未命中删 {changed} 行 in {:?}",
            t.elapsed()
        );
    }

    // 修复后的写法：两段字节范围（两种分隔符）+ 等值自删，形状与新的 remove_under 一致。
    // 先测未命中（移动一个文件时的常见情形）。
    for round in 0..3 {
        let t = Instant::now();
        let a = conn.execute(
            "DELETE FROM files WHERE path >= ?1 AND path < ?2",
            params![format!("{miss}/"), format!("{miss}0")],
        )?;
        let b = conn.execute(
            "DELETE FROM files WHERE path >= ?1 AND path < ?2",
            params![format!("{miss}\\"), format!("{miss}]")],
        )?;
        println!(
            "round {round}: range 未命中删 {a}+{b} 行 in {:?}",
            t.elapsed()
        );
    }
    // 再批量测真实目录前缀（命中）。
    let mut stmt2 = conn.prepare("SELECT path FROM files WHERE is_dir = 1 LIMIT 200")?;
    let many: Vec<String> = stmt2
        .query_map([], |r| r.get::<_, String>(0))?
        .filter_map(|x| x.ok())
        .collect();
    let t = Instant::now();
    let mut hit = 0usize;
    for d in &many {
        hit += conn.execute(
            "DELETE FROM files WHERE path >= ?1 AND path < ?2",
            params![format!("{d}/"), format!("{d}0")],
        )?;
        hit += conn.execute(
            "DELETE FROM files WHERE path >= ?1 AND path < ?2",
            params![format!("{d}\\"), format!("{d}]")],
        )?;
    }
    println!(
        "range 命中 {} 个真实目录前缀，删 {hit} 行，共 {:?}",
        many.len(),
        t.elapsed()
    );

    {
        let mut q = conn.prepare("EXPLAIN QUERY PLAN DELETE FROM files WHERE path LIKE ?1")?;
        let rows = q.query_map(params!["x/%"], |r| r.get::<_, String>(3))?;
        print!("LIKE =>");
        for r in rows.flatten() {
            print!(" | {r}");
        }
        println!();
    }
    {
        let mut q =
            conn.prepare("EXPLAIN QUERY PLAN DELETE FROM files WHERE path >= ?1 AND path < ?2")?;
        let rows = q.query_map(params!["x/", "x0"], |r| r.get::<_, String>(3))?;
        print!("RANGE =>");
        for r in rows.flatten() {
            print!(" | {r}");
        }
        println!();
    }
    {
        let mut q = conn.prepare("EXPLAIN QUERY PLAN DELETE FROM files WHERE path = ?1")?;
        let rows = q.query_map(params!["x"], |r| r.get::<_, String>(3))?;
        print!("EQ =>");
        for r in rows.flatten() {
            print!(" | {r}");
        }
        println!();
    }
    Ok(())
}
