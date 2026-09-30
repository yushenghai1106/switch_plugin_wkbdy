#!/usr/bin/env node
/**
 * 确保 `~/.wb-switch/bin/` 下存在与插件版本一致的内核二进制 `wb-switch`。
 *
 * 设计要点
 * --------
 * - **插件目录按只读处理**：所有写入只落 `~/.wb-switch/`，不往插件安装目录里写东西。
 * - **复用既有 npm 分发链路**：内核二进制已经通过 npm 平台包
 *   `@yushenghai1106/workbuddy-switch-<platform>-<arch>` 分发（见 `.github/workflows/build.yml`），
 *   这里直接装平台包并把二进制复制出来，不重复造下载逻辑。
 * - **版本戳即升级机制**：`plugin.json` 的 version 与 `<bin>/.version` 不一致就重装，
 *   因此插件升级后内核自动跟随，无需用户操作。
 * - **镜像与离线**：`WB_SWITCH_REGISTRY` 可指定 registry（逗号分隔多个，按序尝试）；
 *   默认先官方源再回退 npmmirror。完全离线时用 `WB_SWITCH_BINARY` 指向本地二进制。
 *
 * 作为库被 `mcp-launch.mjs` / `wb-switch.mjs` 复用，也可直接当 CLI 跑（`--quiet`）。
 */

import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const HERE = path.dirname(fileURLToPath(import.meta.url));
/** 插件根目录（本文件位于 `<插件根>/bin/`）。 */
const PLUGIN_ROOT = path.resolve(HERE, "..");

/** 平台 → 平台包内的二进制文件名（与 `npm/scripts/install.js` 保持一致）。 */
const PLATFORM_BIN = {
  "darwin-arm64": "wb-switch-darwin-arm64",
  "darwin-x64": "wb-switch-darwin-x64",
  "win32-x64": "wb-switch-win32-x64.exe",
  "linux-x64": "wb-switch-linux-x64",
  "linux-arm64": "wb-switch-linux-arm64",
};

/** 内核二进制的最小合理体积：明显偏小说明下载/解包出了问题（与 install.js 同判据）。 */
const MIN_BINARY_BYTES = 1024 * 1024;

/**
 * 内核兼容下限：这个版本才引入 `mcp` / `hook-record` / `daemon` 子命令。
 *
 * 为什么要有这道闸：更早的内核**不认识这些子命令**，而它们当时把未知子命令兜底成了
 * `serve`（静默启动一个常驻 web 服务器并一直阻塞）。插件把那种内核拉起来后，
 * 表现是 `mcp` 不响应 / hook 每轮卡到超时——**从输出上完全看不出原因**。
 * 宁可在这里明确报错，也不要让插件被一个不兼容的内核拉起。
 *
 * 注意这道闸只拦「下载路径」：本机已有缓存且版本戳匹配时直接复用（本地开发常用），
 * 不走这里。
 */
const MIN_COMPATIBLE_KERNEL = "0.1.54";

/** 比较 `X.Y.Z` 形式的版本号；`a < b` 返回负数，相等返回 0。非法输入按 0 处理。 */
export function compareVersions(a, b) {
  const parse = (value) =>
    String(value)
      .split(".")
      .map((part) => Number.parseInt(part, 10))
      .map((n) => (Number.isFinite(n) ? n : 0));
  const left = parse(a);
  const right = parse(b);
  for (let i = 0; i < Math.max(left.length, right.length); i += 1) {
    const diff = (left[i] ?? 0) - (right[i] ?? 0);
    if (diff !== 0) return diff < 0 ? -1 : 1;
  }
  return 0;
}

/**
 * 版本是否低于兼容下限；低于时返回可直接抛出的错误文案，否则返回 `null`。
 *
 * 抽成导出函数是为了能被直接测到——这类「只在不兼容时触发」的分支，
 * 靠端到端跑很难覆盖。
 */
export function kernelCompatibilityError(version) {
  if (compareVersions(version, MIN_COMPATIBLE_KERNEL) >= 0) {
    return null;
  }
  return (
    `插件版本 ${version} 低于内核兼容下限 ${MIN_COMPATIBLE_KERNEL}：` +
    `该版本的内核没有 mcp / hook-record 子命令，且未知子命令会被兜底成常驻 web 服务器，` +
    `插件的 hook 会因此每轮卡到超时。` +
    `请先运行 \`sh scripts/bump-version.sh X.Y.Z\` 升版本并发版，让 npm 上存在兼容的内核。`
  );
}

const DEFAULT_REGISTRIES = [
  "https://registry.npmjs.org",
  "https://registry.npmmirror.com",
];

function storeDir() {
  // 必须与 core 的 `config::store_dir()` 用同一个主目录判据：`WB_SWITCH_HOME` 是
  // 「把数据整体挪走」的开关（便携部署 / 隔离），若这里仍用 `os.homedir()`，
  // 内核二进制会留在真实主目录、而内核自己的数据却去了覆盖目录——两边分家。
  const override = process.env.WB_SWITCH_HOME;
  const home = override && override.trim() ? override : os.homedir();
  return path.join(home, ".wb-switch");
}

function binDir() {
  return path.join(storeDir(), "bin");
}

function binPath() {
  return path.join(binDir(), process.platform === "win32" ? "wb-switch.exe" : "wb-switch");
}

function stampPath() {
  return path.join(binDir(), ".version");
}

function runtimePrefix() {
  return path.join(storeDir(), "runtime");
}

function lockDir() {
  return path.join(storeDir(), "runtime.lock");
}

/** 同步睡眠（首次供应是阻塞路径，没有异步上下文可用）。 */
function sleepSync(ms) {
  Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms);
}

/** 插件清单里声明的版本号；读不到则回退为 `unknown`（会导致每次都重装，但不会崩）。 */
function pluginVersion() {
  try {
    const raw = fs.readFileSync(
      path.join(PLUGIN_ROOT, ".codebuddy-plugin", "plugin.json"),
      "utf8",
    );
    const version = JSON.parse(raw).version;
    return typeof version === "string" && version ? version : "unknown";
  } catch {
    return "unknown";
  }
}

function readStamp() {
  try {
    return fs.readFileSync(stampPath(), "utf8").trim();
  } catch {
    return "";
  }
}

/** 二进制已存在且版本戳匹配 → 无需任何网络操作。 */
function isReady(version) {
  if (!fs.existsSync(binPath())) return false;
  if (readStamp() !== version) return false;
  try {
    return fs.statSync(binPath()).size >= MIN_BINARY_BYTES;
  } catch {
    return false;
  }
}

/** 抢一把粗粒度的锁，避免 SessionStart 预热与 MCP 启动并发跑两次 npm install。 */
function acquireLock(timeoutMs) {
  const lock = lockDir();
  const deadline = Date.now() + timeoutMs;
  // 父目录先建好，`mkdir` 才能用「目录已存在」当互斥原语：
  // 注意 `mkdirSync(dir, {recursive:true})` 在目录已存在时**不报错**，拿它做锁会静默失效。
  fs.mkdirSync(storeDir(), { recursive: true });
  for (;;) {
    try {
      fs.mkdirSync(lock);
      return true;
    } catch (error) {
      if (error.code !== "EEXIST") throw error;
      // 僵死锁（进程被杀）不该永久挡住供应：超过 5 分钟直接清掉。
      try {
        if (Date.now() - fs.statSync(lock).mtimeMs > 5 * 60 * 1000) {
          fs.rmSync(lock, { recursive: true, force: true });
          continue;
        }
      } catch {
        continue;
      }
      if (Date.now() > deadline) return false;
      sleepSync(250);
    }
  }
}

function releaseLock() {
  try {
    fs.rmSync(lockDir(), { recursive: true, force: true });
  } catch {
    /* 释放失败不影响正确性，僵死锁有超时清理 */
  }
}

/** 复制二进制到缓存位置并落版本戳。 */
function installFrom(sourcePath, version) {
  fs.mkdirSync(binDir(), { recursive: true });
  const size = fs.statSync(sourcePath).size;
  if (size < MIN_BINARY_BYTES) {
    throw new Error(`内核二进制体积异常（仅 ${size} 字节）：${sourcePath}`);
  }
  // 先写临时文件再改名：避免并发读到半个文件。
  const tmp = `${binPath()}.tmp`;
  fs.copyFileSync(sourcePath, tmp);
  if (process.platform !== "win32") fs.chmodSync(tmp, 0o755);
  fs.renameSync(tmp, binPath());
  fs.writeFileSync(stampPath(), `${version}\n`);
  return binPath();
}

function registries() {
  const fromEnv = (process.env.WB_SWITCH_REGISTRY || "")
    .split(",")
    .map((s) => s.trim())
    .filter(Boolean);
  // 显式指定时**只用**指定的源：企业内网 / 隔离环境里不该再向公网发请求。
  // 未指定时才用默认源，并按序回退。
  return fromEnv.length > 0 ? [...new Set(fromEnv)] : [...DEFAULT_REGISTRIES];
}

/** 用 npm 装平台包（只装平台分包，不装主包，因此不触发 postinstall）。 */
function npmInstallPlatformPackage(pkgName, version, registry) {
  const npmCmd = process.platform === "win32" ? "npm.cmd" : "npm";
  const result = spawnSync(
    npmCmd,
    [
      "install",
      "--prefix",
      runtimePrefix(),
      "--no-save",
      "--no-audit",
      "--no-fund",
      "--loglevel=error",
      "--registry",
      registry,
      `${pkgName}@${version}`,
    ],
    { stdio: "pipe", encoding: "utf8", shell: process.platform === "win32" },
  );
  if (result.error) {
    return { ok: false, reason: result.error.message };
  }
  if (result.status !== 0) {
    const detail = (result.stderr || result.stdout || "").trim().split("\n").slice(-3).join(" ");
    return { ok: false, reason: detail || `npm 退出码 ${result.status}` };
  }
  return { ok: true };
}

/**
 * 确保内核就绪，返回二进制绝对路径。失败抛错（带可操作的中文提示）。
 *
 * @param {{ quiet?: boolean }} [options]
 */
export function ensureRuntime(options = {}) {
  const { quiet = false } = options;
  const log = (msg) => {
    if (!quiet) console.error(`[workbuddy-switch] ${msg}`);
  };

  const version = pluginVersion();

  // 显式指定本地二进制时必须**优先于缓存**：开发 / 离线场景下用户要的是「就用这个」。
  // 若放在 isReady 快速返回之后，同版本号的缓存会把这个覆盖静默吃掉 —— 本地重新编译
  // 出来的内核永远上不了场。
  // 这里每次都重新复制（约十几 MB，本地开发可忽略），换取「改了就是新的」这个确定性。
  const override = process.env.WB_SWITCH_BINARY;
  if (override) {
    const local = path.resolve(override);
    if (!fs.existsSync(local)) {
      throw new Error(`WB_SWITCH_BINARY 指向的文件不存在：${local}`);
    }
    log(`使用 WB_SWITCH_BINARY 指定的二进制：${local}`);
    return installFrom(local, version);
  }

  if (isReady(version)) return binPath();

  // 走到这里说明**要下载内核**：先确认这个版本是可兼容的（见 MIN_COMPATIBLE_KERNEL）。
  // 放在下载前而不是函数开头：本机已有缓存且版本戳匹配时应当直接复用（本地开发常用），
  // 那道闸只该拦住「会引进不兼容内核」的下载路径。
  const incompatible = kernelCompatibilityError(version);
  if (incompatible) {
    throw new Error(incompatible);
  }

  const key = `${process.platform}-${process.arch}`;
  const fileName = PLATFORM_BIN[key];
  if (!fileName) {
    throw new Error(
      `当前平台 ${key} 暂不支持自动供应内核。` +
        `可自行下载二进制后设置 WB_SWITCH_BINARY 指向它。`,
    );
  }

  const pkgName = `@yushenghai1106/workbuddy-switch-${key}`;

  if (!acquireLock(180_000)) {
    // 别的进程正在装：等它装完直接用；仍不可用则报错。
    if (isReady(version)) return binPath();
    throw new Error("等待内核安装超时，请重试。");
  }

  try {
    // 拿锁后复查：并发方可能已经装好了。
    if (isReady(version)) return binPath();

    log(`正在准备内核 v${version}（首次运行需要下载一次）…`);
    const failures = [];
    for (const registry of registries()) {
      const attempt = npmInstallPlatformPackage(pkgName, version, registry);
      if (!attempt.ok) {
        failures.push(`${registry} → ${attempt.reason}`);
        continue;
      }
      const source = path.join(runtimePrefix(), "node_modules", pkgName, "bin", fileName);
      if (!fs.existsSync(source)) {
        failures.push(`${registry} → 平台包内未找到 ${fileName}`);
        continue;
      }
      const installed = installFrom(source, version);
      log(`内核就绪：${installed}`);
      return installed;
    }

    throw new Error(
      [
        `无法获取内核二进制 ${pkgName}@${version}。`,
        ...failures.map((f) => `  - ${f}`),
        "可检查网络/代理，或设置 WB_SWITCH_REGISTRY 指向可用镜像；",
        "完全离线时可设置 WB_SWITCH_BINARY 指向本地二进制。",
      ].join("\n"),
    );
  } finally {
    releaseLock();
  }
}

/** 缓存的内核路径；没有则返回 null（不触发安装）。 */
export function cachedBinaryPath() {
  return fs.existsSync(binPath()) ? binPath() : null;
}

export const paths = { pluginRoot: PLUGIN_ROOT, storeDir, binPath, stampPath };

// 作为 CLI 运行时：只负责“确保就绪”，不产生 stdout 噪声（SessionStart 预热会调用）。
const invokedPath = process.argv[1] ? path.resolve(process.argv[1]) : "";
if (invokedPath === fileURLToPath(import.meta.url)) {
  const quiet = process.argv.includes("--quiet");
  try {
    ensureRuntime({ quiet });
  } catch (error) {
    console.error(`[workbuddy-switch] ${error.message}`);
    process.exit(1);
  }
}
