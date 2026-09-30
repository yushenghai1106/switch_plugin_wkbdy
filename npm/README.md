# workbuddy-switch

WorkBuddy、CodeBuddy IDE、CodeBuddy CLI 与 VS Code CodeBuddy 插件账号切换工具，四者均支持国内版 /
国际版，并提供积分到期与 Token 用量监控。

本包是**内核**：提供命令行与本地 Web 界面（webui）。日常更推荐以插件形态使用——在 CodeBuddy /
WorkBuddy 里 `/plugin marketplace add yushenghai1106/switch_plugin_wkbdy` 安装，能力与 webui 同源，
且不需要单独启动任何程序；插件会按需从本包拉取对应平台的二进制。

多账号共享登录态，一键切换各客户端登录账号。**会话迁移**：把当前对话复制给另一个账号，源账号数据
不受影响，云端归属目标账号。

**在线演示**：[打开 GitHub Pages 演示](https://yushenghai1106.github.io/switch_plugin_wkbdy/)（只读演示；账号、积分与请求记录均为虚构数据，所有业务操作均已禁用）

## 快速开始

```bash
npm i -g @yushenghai1106/workbuddy-switch
workbuddy-switch                # 启动本地服务 + 自动打开浏览器
workbuddy-switch status         # 终端查看当前账号
workbuddy-switch daemon         # 只跑后台任务（签到 / 自动轮换 / 派猫猫旅行 / 限额监听）
workbuddy-switch daemon --stop  # 结束后台任务
```

其它子命令：`serve`（同默认行为，可带 `--port` / `--no-open`）、`mcp`（stdio，供插件调用）、
`hook-record`、`version`。

## 功能

| 模块 | 说明 |
| --- | --- |
| 账号管理 | OAuth 扫码登录、导入本机账号、导入 / 导出备份、删除账号 |
| 账号切换 | 一键切换各客户端登录账号 |
| 导出当前对话 | 把当前对话复制给另一个账号（源账号不变、副本取新 id） |
| 会话复制 | 按 id 把指定会话复制给目标账号，源账号数据不受影响 |
| 积分到期查询 | 自动查询每个账号的积分剩余量与到期时间；7 天内到期高亮并按紧迫程度排序 |
| 积分统计 | 汇总官方请求用量：总览、近 30 天趋势、模型分类、账号消耗与请求明细 |
| Token 统计 | 按来源查看 Token 总览与趋势、构成占比、活跃热力图、项目/模型 Top 10 与会话排行 |
| CodeBuddy CLI | 与 WorkBuddy 复用同一账号库，默认账号独立；切换后立即生效 |
| CodeBuddy IDE | 支持切换 CodeBuddy IDE 桌面客户端账号，与 CodeBuddy CLI 相互独立 |
| VS Code CodeBuddy 插件 | 支持切换 VS Code 内的 CodeBuddy 插件账号；运行时可自动关闭并在写入后重开 |
| JetBrains IDE 插件 | 支持切换 IntelliJ IDEA / PyCharm 内的 CodeBuddy 插件账号 |
| 后台任务 | 守护进程执行周期任务；同一时刻只有一个执行者，可随时查询 / 停止 |
| 权限检测 | macOS 授权引导（App 管理 / 完全磁盘访问拖拽授权 + 自动检测） |

## 使用

1. **添加与导出账号**：账号页 →「OAuth 扫码登录」「导入本机账号」「导入备份」；「导出」可将勾选账号备份为 JSON
2. **切换账号**：账号卡片 →「切换」，可勾选复制当前会话
3. **导出当前对话**：账号卡片 →「导出当前对话」，把当前对话复制给目标账号
4. **查看积分与统计**：账号页自动查询各账号积分到期情况，点「刷新积分」手动更新；侧栏进入「积分统计」「Token 统计」查看用量明细
5. **切换各客户端账号**：CodeBuddy CLI、CodeBuddy IDE、VS Code CodeBuddy 插件均可在账号卡片一键切换；CodeBuddy IDE 首次使用前需先手动打开并登录一次
6. **自动轮换**：设置 → CodeBuddy CLI 自动轮换，开启后按积分紧迫程度自动设置默认账号
7. **更新**：`npm update -g @yushenghai1106/workbuddy-switch`

> 桌面 App 形态已停止发布；原先内置的会话悬浮栏请改用独立的
> [Agent Companion](https://github.com/changexbc/agent-companion)。

## macOS 权限说明

切换账号需要写入 WorkBuddy 认证文件，macOS 要求授权「App 管理」（或「完全磁盘访问」）：

1. 首次切换报「无权限」时，点「打开系统设置」
2. 在 **App 管理** 里打开对应条目；若没有，则去 **完全磁盘访问** 把条目拖进带箭头的框
3. 授权后重启生效；设置页「权限检测」可随时验证

> webui 模式下由启动服务的终端进程权限决定；若终端已授权完全磁盘访问则无需额外操作。
> 插件模式下执行写入的是内核二进制 `~/.wb-switch/bin/wb-switch`。

## 许可

[MIT](./LICENSE)
