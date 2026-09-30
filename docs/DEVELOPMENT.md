# 开发指南

## 环境要求

Node.js ≥ 20、Rust stable。Windows 上还需要 MSVC 工具链与 Windows SDK（Rust 的默认 host
是 `x86_64-pc-windows-msvc`）。

## 项目形态

发布形态是 **插件 + npm 内核**：

- **插件本体**：`plugin/` + 仓库根的 `.codebuddy-plugin/marketplace.json`，随仓库分发，
  用户用 `/plugin marketplace add <owner>/<repo>` 安装。
- **内核二进制**：`crates/wb-switch-server`（package `wb-switch-server`，binary `wb-switch`），
  经 npm 平台包分发，由插件的 `plugin/bin/ensure-runtime.mjs` 首次运行时拉取。
- **桌面 App**：`src-tauri/` 源码保留，但**不再发布安装包**（P6 起）。原先内置的会话悬浮栏
  由独立的 [Agent Companion](https://github.com/changexbc/agent-companion) 承接。

## 开发命令

```bash
npm install
npm run build                        # 前端产物（CLI 二进制经 rust-embed 内嵌它）

cargo test -p wb-switch-core         # 核心逻辑（CI 三平台都跑）
cargo test -p wb-switch-server       # CLI / HTTP / MCP 层
cargo build -p wb-switch-server      # 本地内核 → target/debug/wb-switch

npm run tauri dev                    # 桌面 App（仅本地调试用；已不发布）
```

## 本地校验（与 CI 门禁对齐）

```bash
cargo fmt --check -p wb-switch-core -p wb-switch-server
cargo clippy -p wb-switch-core -p wb-switch-server --all-targets --no-deps -- -D warnings
node scripts/validate-plugin.mjs                      # 插件加载期静态校验
node scripts/validate-plugin.mjs --expect-version=X.Y.Z  # 另校验版本与 tag 一致
```

两点注意：

- **clippy 在 Windows 上会多报几条**：那是既有代码里 Windows 专属 `cfg` 分支的告警，
  CI 的 clippy 跑在 macOS 上、`test.yml` 里明确写了「windows 分支暂未纳入」。
  验证自有代码是否干净，可加 `-A clippy::needless_return -A clippy::unnecessary_mut_passed`
  放行这些既有项，再看输出是否为空。
- **`codebuddy plugin validate` 在 CI 里跑不了**（无 TTY 会静默挂起），所以有
  `scripts/validate-plugin.mjs`：它校验清单位置、`source` 解析基准、`${CODEBUDDY_PLUGIN_ROOT}`
  替换后文件是否存在、版本一致性、frontmatter 等加载期会踩的点。

## ⚠️ 跑真实二进制必须隔离数据目录

`dirs::home_dir()` 在 Windows 上走 `SHGetKnownFolderPath`，**不认 `USERPROFILE`**，
所以唯一可行的隔离手段是 `WB_SWITCH_HOME`：

```bash
WB_SWITCH_HOME=/tmp/wb-isolated ./target/debug/wb-switch serve --no-open
```

不隔离就跑真实二进制，会写进用户的真实环境：`~/.workbuddy/settings.json` 与
`~/.codebuddy/settings.json` 会被装上限额 hook、`~/.wb-switch` 会被写入账号与用量快照、
还会用用户的账号发请求。凡是会执行真实二进制的测试（如
`crates/wb-switch-server/tests/cli.rs`）都必须注入隔离主目录，验证方式是对比运行前后
真实 `~/.wb-switch` 是否逐字节无变化。

## 发布新版本

**第 0 步（最容易漏）**：先 bump 版本。

```bash
sh scripts/bump-version.sh 0.2.0
```

步骤 0 不能省：插件用「插件版本 ≠ 内核缓存版本戳」决定是否重新下载内核，版本没变的话，
用户那边会**继续用旧内核**。CI 的 `validate` job 会用 tag 校验这一点，不一致直接失败。

之后 CI（`.github/workflows/build.yml`）在打 tag 时自动完成：

1. `validate` —— 校验插件清单与版本一致性（tag 必须等于 `plugin.json` 的版本）
2. `build`（4 平台矩阵）—— 构建前端 + `cargo build -p wb-switch-server --release`，
   把二进制发布为 npm 平台包 `@yushenghai1106/workbuddy-switch-<platform>-<arch>`
3. `publish-main` —— 发布 npm 主包（`optionalDependencies` 引用平台包）
4. `release` —— 建一个 Release 作为版本记录（**不再挂安装包**；插件随仓库分发、内核走 npm）

```bash
git tag v0.2.0 && git push origin v0.2.0
```

发布后验证：在客户端里 `/plugin update workbuddy-switch`，或重新 `/plugin install` 后用
`wb_status` 看 `version` 是否为新版本。

### 仓库变量

- `PUBLISH_NPM=false`：跳过两个 npm 发布 job（registry 故障期间用）。注意 `release` job
  刻意**不**依赖 `publish-main`——依赖被跳过时 GitHub 会连带跳过本 job。

## 目录结构

```
plugin/                     # 插件本体（随仓库分发）
  .codebuddy-plugin/plugin.json   # 清单：hooks + mcpServers
  hooks/                    # SessionStart（预热内核 + 拉起守护）、Stop（记录当前对话）
  bin/                      # ensure-runtime / ensure-daemon / mcp-launch / wb-switch 包装
  commands/ skills/         # 斜杠命令与技能
.codebuddy-plugin/          # 市场清单（source 相对仓库根解析）
crates/
  wb-switch-core/           # 核心逻辑（纯 Rust，三宿主共用）
  wb-switch-server/         # CLI + HTTP + MCP（binary: wb-switch）
src-tauri/                  # 桌面 App 源码（已不发布，仅本地调试）
src/                        # 前端（webui 与按需打开的界面）
scripts/                    # 版本 bump、插件校验、打包辅助
npm/                        # npm 包：主包 + 平台包
```

内核子命令：`serve`（HTTP + 前端）、`daemon [--stop]`、`mcp`（stdio）、`hook-record`、
`status`、`version`。未知子命令**报错退出**——历史上有过「兜底成 serve」的实现，
导致旧内核被插件以新子命令拉起时静默启动一个常驻服务器、hook 卡到超时。

## 隐私注意事项

- 仓库不提交本地数据（accounts.json、认证文件、密钥、token 由 `.gitignore` 排除）
- 发布前用 `git grep` 扫描 token 模式（`ghp_`/`npm_`/`gho_` 等）
