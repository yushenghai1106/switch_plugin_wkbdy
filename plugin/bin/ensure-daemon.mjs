#!/usr/bin/env node
/**
 * 确保内核就绪，并以**分离进程**拉起后台守护（`wb-switch daemon`）。
 *
 * 由 SessionStart hook 在后台调用，因此这里可以慢（首次要下载内核），不会占用 hook 的
 * 超时预算；hook 本身只负责把本脚本甩出去就返回。
 *
 * 分离进程的生命周期：守护要跑**账号侧的周期任务**（签到 / 旅行 / 轮换 / 保活），
 * 这些在客户端关掉之后也必须继续，否则「自动轮换在 CLI 启动前把默认账号设好」就不成立。
 * 因此它刻意不随客户端退出。重复拉起是安全的——`daemon` 抢不到跨进程锁时会安静退出。
 *
 * 通篇 fail-open：任何失败只写 stderr、退出码恒为 0，绝不干扰用户的正常会话。
 */

import fs from "node:fs";
import path from "node:path";
import { spawn } from "node:child_process";
import { ensureRuntime, paths } from "./ensure-runtime.mjs";

/**
 * 后台周期任务是否开启（读 `<store>/daemon_config.json`，缺失/损坏按开启处理）。
 *
 * 这里先判一次是为了**根本不 spawn**：内核侧（`daemon::start_if_elected`）也会拦，
 * 但那样每次会话都会白起一个进程再退出，还会在 daemon.log 里留噪声。
 * 默认值必须与 core 的 `default_daemon_config()` 保持一致（true）。
 */
function backgroundTasksEnabled() {
  try {
    const raw = fs.readFileSync(path.join(paths.storeDir(), "daemon_config.json"), "utf8");
    const value = JSON.parse(raw).backgroundTasks;
    return typeof value === "boolean" ? value : true;
  } catch {
    return true;
  }
}

let binary;
try {
  binary = ensureRuntime({ quiet: true });
} catch (error) {
  process.stderr.write(
    `[workbuddy-switch] 内核不可用，后台任务未启动：${error.message}\n`,
  );
  process.exit(0);
}

// 用户在配置里关掉了后台周期任务：内核仍然预热（MCP 工具还要用它），但不拉守护。
if (!backgroundTasksEnabled()) {
  process.stderr.write(
    "[workbuddy-switch] 后台周期任务已在配置中关闭，本次不启动守护进程。\n",
  );
  process.exit(0);
}

try {
  // 守护的输出落到 `<store>/daemon.log`：分离进程没法回传 stdout，
  // 出问题时这行日志是唯一的线索。只在启动与被推迟的轮换上写，量很小。
  //
  // 目录判据必须复用 `ensure-runtime` 的 `storeDir()`（它认 `WB_SWITCH_HOME`）：
  // 这里若自己拼 `os.homedir()`，便携 / 隔离部署下内核数据搬走了、日志却留在真实主目录。
  const storeDir = paths.storeDir();
  fs.mkdirSync(storeDir, { recursive: true });
  const logFd = fs.openSync(path.join(storeDir, "daemon.log"), "a");
  try {
    const child = spawn(binary, ["daemon"], {
      detached: true,
      stdio: ["ignore", logFd, logFd],
      windowsHide: true,
    });
    child.unref();
  } finally {
    // 父进程这一份 fd 用完即关；子进程持有自己的副本，不受影响。
    fs.closeSync(logFd);
  }
} catch (error) {
  process.stderr.write(`[workbuddy-switch] 启动后台任务失败：${error.message}\n`);
}

process.exit(0);
