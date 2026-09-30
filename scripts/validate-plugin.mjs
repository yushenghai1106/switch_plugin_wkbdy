#!/usr/bin/env node
/**
 * 插件加载期静态校验。
 *
 * 为什么需要它：官方 `codebuddy plugin validate` 在非交互环境（无 TTY）下会静默挂起、
 * 零输出，CI 里根本跑不了；而插件最常见的失败恰恰是**路径解析**——清单位置、
 * `source` 基准目录、`${CODEBUDDY_PLUGIN_ROOT}` 替换后文件在不在。这些都能在纯静态
 * 层面查出来，不必真的把插件装进客户端。
 *
 * 校验规则全部来自真实已装插件的实测结论（见 `.codebuddy-plugin/marketplace.json`
 * 与 `~/.codebuddy/plugins/marketplaces/*`）：
 * - 市场清单固定在 `<仓库根>/.codebuddy-plugin/marketplace.json`；
 * - 条目的 `source` 相对**市场根**（即 `.codebuddy-plugin/` 的父目录）解析，
 *   两个官方市场都是 `"./plugins/x"` → `<市场根>/plugins/x`；
 * - 插件清单固定在 `<插件根>/.codebuddy-plugin/plugin.json`；
 * - `hooks` 等组件路径、`mcpServers` 的 `args` 都相对**插件根**解析。
 *
 * 用法：`node scripts/validate-plugin.mjs`（退出码非 0 表示校验失败）
 */

import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const REPO_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

let failures = 0;
const ok = (msg) => console.log(`  \u2713 ${msg}`);
const bad = (msg) => {
  console.log(`  \u2717 ${msg}`);
  failures += 1;
};
const section = (title) => console.log(`\n${title}`);

/** 把 `${CODEBUDDY_PLUGIN_ROOT}` / `${CLAUDE_PLUGIN_ROOT}` 换成真实插件根。 */
function expandPlaceholders(value, pluginRoot) {
  return value
    .replaceAll("${CODEBUDDY_PLUGIN_ROOT}", pluginRoot)
    .replaceAll("${CLAUDE_PLUGIN_ROOT}", pluginRoot);
}

function readJson(file) {
  return JSON.parse(fs.readFileSync(file, "utf8"));
}

section("[1] 市场清单");
const marketFile = path.join(REPO_ROOT, ".codebuddy-plugin", "marketplace.json");
if (!fs.existsSync(marketFile)) {
  bad(`缺少 ${path.relative(REPO_ROOT, marketFile)}`);
  process.exit(1);
}
ok(".codebuddy-plugin/marketplace.json 存在");
const market = readJson(marketFile);
if (!market.name || !Array.isArray(market.plugins) || market.plugins.length === 0) {
  bad("市场清单必须含 name 与非空 plugins");
  process.exit(1);
}
ok(`市场名 ${market.name}，含 ${market.plugins.length} 个插件条目`);

/** 市场根 = 市场清单所在目录的父目录；`source` 相对它解析。 */
const marketRoot = path.dirname(path.dirname(marketFile));

section("[2] 插件条目 → source 解析（基准是市场根）");
const entry = market.plugins.find((p) => p.name === "workbuddy-switch");
if (!entry) {
  bad("市场里没有 workbuddy-switch 条目");
  process.exit(1);
}
if (typeof entry.source !== "string" || !entry.source.startsWith("./")) {
  bad(`source 必须是相对市场根的 "./..." 形式，当前为 ${JSON.stringify(entry.source)}`);
} else {
  ok(`source = ${entry.source}`);
}
const pluginRoot = path.resolve(marketRoot, entry.source);
if (fs.existsSync(pluginRoot)) ok(`解析到 ${path.relative(REPO_ROOT, pluginRoot)}（存在）`);
else bad(`解析到 ${pluginRoot}（不存在）`);

section("[3] 插件清单位置");
const pluginFile = path.join(pluginRoot, ".codebuddy-plugin", "plugin.json");
if (!fs.existsSync(pluginFile)) {
  bad(`缺少 ${path.relative(REPO_ROOT, pluginFile)}（必须在 <插件根>/.codebuddy-plugin/ 下）`);
  process.exit(1);
}
ok(".codebuddy-plugin/plugin.json 存在");
const plugin = readJson(pluginFile);
if (!plugin.name || !plugin.version) bad("plugin.json 必须含 name 与 version");
else ok(`插件名 ${plugin.name}，版本 ${plugin.version}`);

section("[4] 版本一致性");
if (entry.version !== plugin.version) {
  bad(`marketplace 条目版本 ${entry.version} 与 plugin.json 的 ${plugin.version} 不一致`);
} else if (market.metadata?.version && market.metadata.version !== plugin.version) {
  bad(`marketplace metadata.version ${market.metadata.version} 与插件版本不一致`);
} else {
  ok(`marketplace / metadata / plugin.json 版本一致：${plugin.version}`);
}

// 可选：与发布 tag 对齐。插件的 `ensure-runtime` 用「插件版本 ≠ 内核缓存版本戳」来决定
// 是否重新拉内核，所以**发版必须 bump 版本**；这里直接拦住「tag 与插件版本不一致」的包。
const expectedVersion = process.argv
  .find((arg) => arg.startsWith("--expect-version="))
  ?.slice("--expect-version=".length);
if (expectedVersion) {
  if (expectedVersion === plugin.version) {
    ok(`与期望版本一致：${expectedVersion}`);
  } else {
    bad(
      `插件版本 ${plugin.version} 与期望版本 ${expectedVersion} 不一致。` +
        `发布前请先运行 scripts/bump-version.sh ${expectedVersion}——版本不变的话，` +
        `插件的 ensure-runtime 会认为内核已就绪，继续使用旧内核。`,
    );
  }
}

section("[5] hooks 声明与其命令里的脚本");
if (!plugin.hooks) {
  bad("plugin.json 未声明 hooks");
} else {
  const hooksFile = path.resolve(pluginRoot, plugin.hooks);
  if (!fs.existsSync(hooksFile)) {
    bad(`hooks 指向 ${plugin.hooks}，但文件不存在`);
  } else {
    ok(`hooks -> ${plugin.hooks}（存在）`);
    const hooks = readJson(hooksFile);
    const events = Object.entries(hooks.hooks ?? {});
    if (events.length === 0) bad("hooks.json 里没有任何事件");
    for (const [event, groups] of events) {
      for (const group of groups) {
        const commands = (group.hooks ?? []).filter((h) => h.type === "command");
        if (commands.length === 0) {
          // `type: "prompt"` 或空数组是合法的，不算失败。
          continue;
        }
        for (const hook of commands) {
          checkHookCommand(event, hook.command);
        }
      }
    }
  }
}

/** 校验一条 hook 命令：占位符替换后，其中的脚本路径必须真实存在。 */
function checkHookCommand(event, command) {
  if (!command.includes("${CODEBUDDY_PLUGIN_ROOT}") && !command.includes("${CLAUDE_PLUGIN_ROOT}")) {
    bad(`${event} 的命令未使用插件根占位符，插件装到别处就会失效：${command}`);
    return;
  }
  const expanded = expandPlaceholders(command, pluginRoot);
  const referenced = expanded.match(/["']?([^"'\s]+\.(?:mjs|cjs|js|sh|cmd|ps1))["']?/g) ?? [];
  if (referenced.length === 0) {
    // 内联命令（如 `node -e "..."`）没有独立脚本文件，跳过路径检查。
    ok(`${event} 使用内联命令（无独立脚本文件）`);
    return;
  }
  for (const raw of referenced) {
    const file = raw.replace(/^["']|["']$/g, "");
    if (fs.existsSync(file)) ok(`${event} -> ${path.relative(pluginRoot, file)}（存在）`);
    else bad(`${event} 引用的脚本不存在：${file}`);
  }
}

section("[6] mcpServers 声明");
const server = plugin.mcpServers?.["wb-switch"];
if (!server) {
  bad("plugin.json 未声明 mcpServers['wb-switch']");
} else {
  if (server.command !== "node") {
    // 用解释器而不是直接指向平台二进制：跨平台一致，且不依赖插件的可执行位。
    bad(`mcpServers.command 应为 node（跨平台稳妥），当前为 ${server.command}`);
  } else {
    ok("command = node");
  }
  const args = server.args ?? [];
  if (args.length === 0) bad("mcpServers.args 为空");
  for (const arg of args) {
    if (!arg.includes("${CODEBUDDY_PLUGIN_ROOT}") && !arg.includes("${CLAUDE_PLUGIN_ROOT}")) {
      bad(`args 里的路径未使用插件根占位符：${arg}`);
      continue;
    }
    const expanded = expandPlaceholders(arg, pluginRoot);
    if (fs.existsSync(expanded)) ok(`args -> ${path.relative(pluginRoot, expanded)}（存在）`);
    else bad(`args 替换后不存在：${expanded}`);
  }
}

section("[7] 供 launcher 依赖的运行时脚本");
for (const rel of ["bin/ensure-runtime.mjs", "bin/mcp-launch.mjs"]) {
  const file = path.join(pluginRoot, rel);
  if (fs.existsSync(file)) ok(`${rel} 存在`);
  else bad(`${rel} 缺失（launcher 依赖它）`);
}

section("[8] commands / skills / agents 的 frontmatter");
checkMarkdownDir("commands", /^---\r?\n[\s\S]*?description:\s*\S[\s\S]*?\r?\n---/);
checkMarkdownDir("agents", /^---\r?\n[\s\S]*?description:\s*\S[\s\S]*?\r?\n---/);
// skills 既支持 `skills/<名>/SKILL.md`，也支持根级 SKILL.md。
const skillsDir = path.join(pluginRoot, "skills");
if (fs.existsSync(skillsDir)) {
  for (const dirent of fs.readdirSync(skillsDir, { withFileTypes: true })) {
    if (!dirent.isDirectory()) continue;
    const skill = path.join(skillsDir, dirent.name, "SKILL.md");
    if (!fs.existsSync(skill)) bad(`skills/${dirent.name}/ 下缺少 SKILL.md`);
    else checkFrontmatter(skill, /^---\r?\n[\s\S]*?name:\s*\S[\s\S]*?description:\s*\S[\s\S]*?\r?\n---/, "name/description");
  }
}

function checkMarkdownDir(rel, pattern) {
  const dir = path.join(pluginRoot, rel);
  if (!fs.existsSync(dir)) return;
  for (const file of fs.readdirSync(dir).filter((f) => f.endsWith(".md"))) {
    checkFrontmatter(path.join(dir, file), pattern, "description");
  }
}

function checkFrontmatter(file, pattern, required) {
  const text = fs.readFileSync(file, "utf8");
  const rel = path.relative(pluginRoot, file).replaceAll("\\", "/");
  if (pattern.test(text)) ok(`${rel} frontmatter 含 ${required}`);
  else bad(`${rel} frontmatter 缺少 ${required}`);
}

console.log("");
if (failures === 0) {
  console.log("插件加载期静态校验全部通过。");
  process.exit(0);
}
console.log(`插件加载期静态校验失败：${failures} 项。`);
process.exit(1);
