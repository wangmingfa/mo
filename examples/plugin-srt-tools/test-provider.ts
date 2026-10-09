#!/usr/bin/env bun
// srt-tools provider 协议自测（不依赖 Mo）。
// 自己用 bun 拉起 bin/srt-tools.ts，逐帧发 initialize / classify / preview / list /
// 未知方法，断言回包正确。
//
// 运行：bun test_provider.ts

import { spawn } from "bun";

const PROVIDER = new URL("./bin/srt-tools.ts", import.meta.url).pathname;

let passed = 0;
let failed = 0;

function check(cond: boolean, what: string): void {
  if (cond) {
    passed++;
    console.log(`  ✓ ${what}`);
  } else {
    failed++;
    console.error(`  ✗ ${what}`);
  }
}

const proc = spawn(["bun", PROVIDER], {
  stdout: "pipe",
  stdin: "pipe",
  stderr: "ignore",
});

const decoder = new TextDecoder();
let buffer = "";
const waiters = new Map<number, (msg: any) => void>();
let nextId = 1;

(async () => {
  const reader = (proc.stdout as ReadableStream<Uint8Array>).getReader();
  while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    buffer += decoder.decode(value, { stream: true });
    let nl: number;
    while ((nl = buffer.indexOf("\n")) >= 0) {
      const line = buffer.slice(0, nl).trim();
      buffer = buffer.slice(nl + 1);
      if (!line) continue;
      const msg = JSON.parse(line);
      const cb = waiters.get(msg.id);
      if (cb) {
        waiters.delete(msg.id);
        cb(msg);
      }
    }
  }
})();

function call(method: string, params: Record<string, any>): Promise<any> {
  const id = nextId++;
  return new Promise((resolve) => {
    waiters.set(id, resolve);
    proc.stdin.write(JSON.stringify({ id, method, params }) + "\n");
  });
}

async function main(): Promise<void> {
  console.log("provider 协议自测（bun）");

  // 1) 握手
  const init = await call("initialize", { protocol: 1 });
  check(
    init.result &&
      init.result.name === "srt-tools" &&
      Array.isArray(init.result.methods) &&
      init.result.methods.includes("classify"),
    "initialize → 报 name/methods"
  );

  // 2) classify：.srt
  const c1 = await call("classify", {
    path: "/m/movie.srt",
    name: "movie.srt",
    size: 1234,
  });
  check(c1.result?.label === "SRT 字幕", "classify .srt → 『SRT 字幕』");

  // 3) classify：.vtt
  const c2 = await call("classify", {
    path: "/m/movie.vtt",
    name: "movie.vtt",
    size: 999,
  });
  check(c2.result?.label === "WebVTT 字幕", "classify .vtt → 『WebVTT 字幕』");

  // 4) classify：不认识 → 空对象（Mo 回落内置）
  const c3 = await call("classify", { path: "/m/x.mp4", name: "x.mp4", size: 1 });
  check(
    c3.result && (c3.result.label === undefined),
    "classify .mp4 → 空（回落内置）"
  );

  // 5) preview：用真实临时 .srt 文件
  const tmp = `/tmp/mo-srt-test-${Date.now()}.srt`;
  await Bun.write(
    tmp,
    "1\n00:00:01,000 --> 00:00:04,000\n你好世界\n\n2\n00:00:05,000 --> 00:00:08,000\n第二句"
  );
  const p1 = await call("preview", {
    path: tmp,
    name: "movie.srt",
    kind: "text",
    max_bytes: 524288,
  });
  check(
    p1.result?.kind === "text" && typeof p1.result?.text === "string" && p1.result.text.includes("你好世界"),
    "preview .srt → text 摘要"
  );

  // 6) preview：非字幕 → unsupported
  const p2 = await call("preview", {
    path: "/m/x.png",
    name: "x.png",
    kind: "image",
    max_bytes: 524288,
  });
  check(p2.result?.kind === "unsupported", "preview 非字幕 → unsupported");

  // 7) list → 至少一行
  const l1 = await call("list", { source: "recent" });
  check(
    Array.isArray(l1.result?.rows) && l1.result.rows.length >= 1,
    "list → 至少一行"
  );

  // 8) 未知方法 → error 且不崩
  const e1 = await call("frobnicate", {});
  check(typeof e1.error === "string", "未知方法 → error（进程不崩）");

  proc.kill();
  console.log(`\n结果：${passed} 通过 / ${failed} 失败`);
  // 管道下 process.exit 会截断 stdout，先等冲刷完（见 bun/node 已知行为）。
  await new Promise((r) => setTimeout(r, 50));
  process.exit(failed === 0 ? 0 : 1);
}

main();
