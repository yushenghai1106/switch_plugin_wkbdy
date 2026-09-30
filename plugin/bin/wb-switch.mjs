#!/usr/bin/env node
/**
 * 内核通用包装器：供命令 / 技能里手动调用（例如 `node <插件根>/bin/wb-switch.mjs accounts`）。
 *
 * 与 `mcp-launch.mjs` 的区别：这里是一次性 CLI 调用，stdout 直接透传给调用方（AI 读取），
 * 因此不吞输出。内核本身就是 `wb-switch` CLI，子命令语义见 `crates/wb-switch-server/src/main.rs`。
 */

import { spawnSync } from "node:child_process";
import { ensureRuntime } from "./ensure-runtime.mjs";

let binary;
try {
  binary = ensureRuntime();
} catch (error) {
  console.error(`[workbuddy-switch] 内核不可用：${error.message}`);
  process.exit(1);
}

const result = spawnSync(binary, process.argv.slice(2), {
  stdio: "inherit",
  windowsHide: true,
});

if (result.error) {
  console.error(`[workbuddy-switch] 执行内核失败：${result.error.message}`);
  process.exit(1);
}
process.exit(result.status ?? 1);
