# workbuddy-switch 插件

把 [workbuddy-switch](https://github.com/yushenghai1106/switch_plugin_wkbdy) 的账号切换与会话复制能力
接进 CodeBuddy / WorkBuddy，无需再单独启动桌面 App。

## 结构

```
.codebuddy-plugin/plugin.json   # 插件清单：声明 hooks 与 mcpServers
hooks/hooks.json                # 事件注册（SessionStart / Stop）
hooks/session-start.mjs         # 甩出 ensure-daemon（detached，不占 hook 超时）
hooks/record.mjs                # Stop：记录「当前对话」（只用已缓存内核，绝不触发下载）
bin/ensure-runtime.mjs          # 确保 ~/.wb-switch/bin/wb-switch 就绪，版本随插件走
bin/ensure-daemon.mjs           # 内核就绪后拉起后台守护（detached，幂等；配置关闭时不拉起）
bin/mcp-launch.mjs              # mcpServers 入口：确保内核后 spawn，stdio 透传
bin/wb-switch.mjs               # 一次性 CLI 包装（供命令 / 技能手动调用）
commands/                       # 斜杠命令
skills/                         # 技能
```

## 后台任务（守护进程）

签到 / 派猫猫旅行 / 自动轮换 / 保活 / 限额 hook 监听由 `wb-switch daemon` 执行。
它在会话启动时由 hook 自动拉起，**不随客户端退出**——这些是账号侧的周期任务，
客户端关掉后仍要继续（否则「自动轮换在 CLI 启动前设好默认账号」就不成立）。

- 同一时刻只有一个执行者：桌面版、`wb-switch serve`、守护进程共用 `<store>/daemon.lock`
  这把跨进程锁，谁先拿到谁跑，其余宿主只提供自己的服务能力。
  （刻意**不**复用桌面版的 `instance.lock`——那个锁的语义是「已有实例就退出进程」，
  守护持锁会让桌面版被误判成第二实例。）
- 当前 PID 记在 `~/.wb-switch/daemon.pid`（诊断用；判据始终是锁，不是这个文件）。
- 临时停止：`wb-switch daemon --stop`，或在客户端里调用 `wb_daemon`（`action=stop`）。
  只停进程是不够的——下次会话启动还会被拉起来。
- **彻底关闭**：`wb_daemon` 的 `action=disable`，写的是 `<store>/daemon_config.json` 的
  `backgroundTasks: false`；此后 hook 不再拉起守护。只停周期任务，账号切换 / 导出对话 /
  查询统计等**按需能力不受影响**。恢复用 `action=enable`。
- 守护的输出落在 `<store>/daemon.log`。

## 内核二进制从哪来

插件本身**不带二进制**（否则要按 5 个平台分别发包）。首次运行由 `bin/ensure-runtime.mjs`
从 npm 平台包 `@yushenghai/workbuddy-switch-<platform>-<arch>` 拉取，落到 `~/.wb-switch/bin/`，
并用 `.version` 与 `plugin.json` 的版本比对来驱动升级。

环境变量：

| 变量 | 用途 |
| --- | --- |
| `WB_SWITCH_BINARY` | 指向本地二进制，跳过下载（离线 / 开发调试） |
| `WB_SWITCH_REGISTRY` | 指定 registry，逗号分隔多个按序尝试；默认官方源 + npmmirror 回退 |
| `WB_SWITCH_HOME` | 覆盖用户主目录，把 `~/.wb-switch` 与各客户端数据目录整体挪到指定位置（便携部署 / 隔离测试） |

> `WB_SWITCH_HOME` 是 Windows 上**唯一**能隔离数据目录的手段：`dirs::home_dir()` 在那个平台走
> `SHGetKnownFolderPath`，不认 `USERPROFILE`。

## 本地开发与自测

```bash
# 1) 先用本地编译的二进制跑通，避免依赖已发布的 npm 包
cargo build -p wb-switch-server
WB_SWITCH_BINARY=$PWD/target/debug/wb-switch node plugin/bin/ensure-runtime.mjs

# 2) 加本地市场并安装（在 CodeBuddy / WorkBuddy 里执行）
#    /plugin marketplace add <本仓库路径>
#    /plugin install workbuddy-switch@wb-switch-market
#    /reload-plugins

# 3) 验证 MCP 服务器能应答（手工发一条 initialize）
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05"}}' \
  | node plugin/bin/mcp-launch.mjs
```

> 注意：hooks 配置在客户端**启动时**被读取，改动 `hooks.json` 后必须完全重启客户端
> （WorkBuddy 关窗口不等于退出）。

## 版本同步

`plugin/.codebuddy-plugin/plugin.json` 与仓库根 `.codebuddy-plugin/marketplace.json` 的版本
必须与其它清单一致，`scripts/bump-version.sh` 会一并更新。
