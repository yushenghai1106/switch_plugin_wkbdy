#!/usr/bin/env node
/**
 * Stop hook：把「当前是哪个对话」交给内核记录，供「一键导给另一个号」使用。
 *
 * 三条硬约束，都是为了不给用户添麻烦：
 *
 * 1. **fail-open**：记录失败只写 stderr，stdout 固定回 `{}`、退出码恒为 0。
 *    客户端会把空 stdout 当作 hook 失败，所以必须出声；`{}` 对协议是中性的。
 *
 * 2. **绝不在 hook 里供应内核**：这里只用已缓存的二进制（`cachedBinaryPath`）。
 *    若换成会触发安装的路径，首次运行时这个 hook 会卡在下载上直到超时，而 Stop 是
 *    每轮对话结束都会跑的——那等于每轮都卡一次。预热交给 SessionStart 与 MCP 启动，
 *    内核还没就绪时本轮直接跳过即可（记不了就当没有当前对话，功能有回退路径）。
 *
 * 3. **不阻塞等待**：即使内核存在，也只做一次 spawn 就返回。
 */

import fs from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { cachedBinaryPath } from "../bin/ensure-runtime.mjs";

/** hook 约定的中性响应：必须写，且内容不影响协议。 */
function respond() {
  process.stdout.write("{}\n");
}

/** 消费 stdin：hook 把 payload 写在 stdin 上，读干净再退出，避免上游写管道阻塞。 */
function readPayload() {
  try {
    return fs.readFileSync(0, "utf8");
  } catch {
    return "";
  }
}

const binary = (() => {
  try {
    return cachedBinaryPath();
  } catch {
    return null;
  }
})();

if (binary) {
  const payload = readPayload();
  try {
    const result = spawnSync(binary, ["hook-record"], {
      input: payload,
      // 内核的 stdout 丢弃：它自己也会回一个 `{}`，若原样透传，客户端会看到两行。
      // 本脚本统一负责对外只发一行 `{}`（见 respond）。
      stdio: ["pipe", "ignore", "inherit"],
      windowsHide: true,
    });
    if (result.error) {
      process.stderr.write(
        `[workbuddy-switch] 记录当前对话失败：${result.error.message}\n`,
      );
    }
  } catch (error) {
    process.stderr.write(`[workbuddy-switch] 记录当前对话失败：${error.message}\n`);
  }
} else {
  // 内核尚未就绪：不在这里装，静默跳过（SessionStart / MCP 启动会把它准备好）。
  readPayload();
}

respond();
