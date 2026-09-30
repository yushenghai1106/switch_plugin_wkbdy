#!/usr/bin/env node
/**
 * MCP 服务器入口（`plugin.json` 的 `mcpServers` 指向本文件）。
 *
 * 为什么不直接把 `mcpServers.command` 指向内核二进制：
 * - Windows 下 `command` 若指向 `.cmd` 会依赖 shell 解析，行为不稳定；
 * - 插件安装目录可能没有可执行位（`/plugin install` 复制后权限不可控）。
 * 用 `node` 作 command、由本脚本确保内核就绪再 `spawn`，跨平台一致且首次运行能自动下载。
 *
 * stdio 用 `inherit` 透传：MCP 的 JSON-RPC 直接走本进程的 stdin/stdout，
 * 本脚本不解析、不缓冲，避免破坏协议。
 */

import { spawn } from "node:child_process";
import { ensureRuntime } from "./ensure-runtime.mjs";

function fail(message) {
  // stderr 不是 MCP 的协议通道，写这里客户端会当日志收集，用户能看到原因。
  console.error(`[workbuddy-switch] ${message}`);
  process.exit(1);
}

let binary;
try {
  binary = ensureRuntime();
} catch (error) {
  fail(`内核不可用，MCP 服务器无法启动：${error.message}`);
}

const args = process.argv.slice(2);

// Windows 上 spawn 一个不可执行/损坏的文件会**同步抛出**（不是触发 'error' 事件），
// 所以这里必须包 try/catch，否则内核异常时整个 launcher 会以未捕获异常崩掉，
// 客户端只能看到一段堆栈。
let child;
try {
  child = spawn(binary, args.length > 0 ? args : ["mcp"], {
    stdio: "inherit",
    windowsHide: true,
  });
} catch (error) {
  fail(`启动内核失败（${binary}）：${error.message}`);
}

child.on("error", (error) => fail(`启动内核失败：${error.message}`));
child.on("exit", (code, signal) => process.exit(code ?? (signal ? 1 : 0)));

// 客户端退出 / 用户中断时把信号转给内核，避免残留进程。
for (const signal of ["SIGINT", "SIGTERM", "SIGHUP"]) {
  try {
    process.on(signal, () => {
      if (!child.killed) child.kill(signal);
    });
  } catch {
    /* 当前平台不支持该信号：忽略 */
  }
}
