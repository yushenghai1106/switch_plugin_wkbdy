#!/usr/bin/env node
/**
 * SessionStart hook：把「内核就绪 + 后台守护」预热到后台。
 *
 * 为什么必须 detached：hook 有 `timeout`（本插件 15 秒），而首次拉取平台包可能需要
 * 数十秒。同步等待会拖慢会话启动、甚至被判超时。因此本脚本只做一件事——把
 * `ensure-daemon.mjs` 作为独立进程甩出去，然后立刻退出。
 *
 * 为什么预热和守护是同一个脚本：守护依赖内核。分两步（先预热、下次会话再拉守护）会让
 * 「第一次装上插件的那一轮会话没有后台任务」，用户看到的就是签到/轮换没生效。
 * 串在同一个分离进程里，顺序天然成立。
 *
 * 输出约定：SessionStart 的 stdout 会被当作 `additionalContext` 注入模型上下文
 * （见官方插件 financial-analysis 的实现）。这里不注入任何内容，因此**不写 stdout**，
 * 直接 exit 0。
 *
 * 失败一律静默：预热失败不该阻塞用户正常使用；真正需要内核时（MCP 启动 / 命令调用）
 * 会再走一次 ensure-runtime 并给出明确报错。
 */

import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";
import path from "node:path";

const here = path.dirname(fileURLToPath(import.meta.url));
const ensureScript = path.join(here, "..", "bin", "ensure-daemon.mjs");

/** 消费 stdin：hook 会把 payload 写进 stdin，不读干净可能让上游写管道阻塞。 */
function drainStdin() {
  try {
    process.stdin.resume();
    process.stdin.on("data", () => {});
    process.stdin.on("end", () => {});
    process.stdin.on("error", () => {});
  } catch {
    /* 没有 stdin 时（手动执行）忽略 */
  }
}

drainStdin();

try {
  const child = spawn(process.execPath, [ensureScript], {
    detached: true,
    stdio: "ignore",
    windowsHide: true,
  });
  child.unref();
} catch {
  /* 预热失败静默：后续调用会重试并报错 */
}

// 立刻结束：不在 stdin 上等待，避免 hook 挂到超时。
process.exit(0);
