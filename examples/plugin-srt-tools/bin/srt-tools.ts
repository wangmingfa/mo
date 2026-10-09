#!/usr/bin/env bun
// Mo 插件 provider：srt-tools（字幕工具）
//
// 协议：宿主 ↔ 插件 之间用「每行一条 JSON」的 JSON Lines 经 stdin/stdout 通信。
// 日志一律走 stderr，绝不写 stdout（否则会污染协议通道、串帧）。
//
// 本文件是 Mo 真正拉起来跑的进程（清单里 provider.run = ["bin/srt-tools.ts"]，
// Mo 会把它按扩展目录解析成绝对路径，靠首行 shebang 由 bun 直接执行，无需编译）。
//
// 四种帧：initialize / classify / preview / list。

type Json = Record<string, any>;

interface Request {
  id: number;
  method: string;
  params: Json;
}

// 清单声明支持的方法；initialize 之后宿主会核对，不在表里的会被忽略。
const METHODS = ["classify", "preview", "list"];

function log(...args: any[]): void {
  console.error("[srt-tools]", ...args);
}

// 握手：报名字 / 版本 / 支持的方法。
function initialize(_params: Json): Json {
  return { name: "srt-tools", version: "1.0.0", methods: METHODS };
}

// 种类：对 .srt / .vtt 回中文标签，让文件「种类」列显示成「SRT 字幕」而不是 SubRip。
// 不认识的扩展名就回空对象 {}，Mo 会回落到内置种类判断。
function classify(params: Json): Json {
  const name: string = params.name ?? "";
  const lower = name.toLowerCase();
  if (lower.endsWith(".srt")) return { label: "SRT 字幕" };
  if (lower.endsWith(".vtt")) return { label: "WebVTT 字幕" };
  return {};
}

// 预览：只读文本字幕，取前若干行拼成摘要回文本；非字幕或非文本回 unsupported，
// Mo 会回落到内置预览（这正是「provider 卡死，内置预览照常」的兜底）。
async function preview(params: Json): Promise<Json> {
  const path: string = params.path ?? "";
  const maxBytes: number =
    typeof params.max_bytes === "number" ? params.max_bytes : 512 * 1024;

  if (!/\.(srt|vtt)$/i.test(path)) return { kind: "unsupported" };

  let text: string;
  try {
    const file = Bun.file(path);
    text = await file.text();
  } catch {
    return { kind: "unsupported" };
  }

  const lines = text.split(/\r?\n/);
  const shown = Math.min(lines.length, 60);
  const head = lines.slice(0, shown).join("\n").slice(0, maxBytes);
  const summary =
    `字幕文件预览（前 ${shown} 行 / 共 ${lines.length} 行）：\n\n` + head;
  return { kind: "text", text: summary };
}

// 列表源（只读）：侧栏里的一行 + 点进去的只读面板。这里给两个示例工具入口，
// 不挂 path，所以是「不动的一行」（双击不会跳转到文件）。
function list(_params: Json): Json {
  return {
    rows: [
      {
        id: "check",
        name: "字幕时间轴检查",
        subtitle: "检查 .srt 时间轴是否连续、无重叠",
      },
      {
        id: "convert",
        name: "SRT → VTT 转换",
        subtitle: "把目录下 .srt 批量转成 .vtt",
      },
    ],
  };
}

const handlers: Record<string, (p: Json) => Json | Promise<Json>> = {
  initialize,
  classify,
  preview,
  list,
};

function respond(id: number, payload: { result?: Json; error?: string }): void {
  process.stdout.write(JSON.stringify({ id, ...payload }) + "\n");
}

async function handle(req: Request): Promise<void> {
  const fn = handlers[req.method];
  if (!fn) {
    respond(req.id, { error: `unknown method: ${req.method}` });
    return;
  }
  try {
    const result = await fn(req.params ?? {});
    respond(req.id, { result });
  } catch (e) {
    respond(req.id, { error: String(e) });
  }
}

// ---- 主循环：逐行读 stdin，按 id 派发并回包 ----
// Bun.stdin 是 BunFile，`.stream()` 方法返回 Web ReadableStream（可直接 for await）。
const decoder = new TextDecoder();
let buffer = "";

for await (const chunk of Bun.stdin.stream() as ReadableStream<Uint8Array>) {
  buffer += decoder.decode(chunk, { stream: true });
  let nl: number;
  while ((nl = buffer.indexOf("\n")) >= 0) {
    const line = buffer.slice(0, nl).trim();
    buffer = buffer.slice(nl + 1);
    if (!line) continue;
    try {
      const req = JSON.parse(line) as Request;
      if (typeof req.id === "number") {
        await handle(req);
      }
    } catch (e) {
      log("丢弃无法解析的行:", e);
    }
  }
}
