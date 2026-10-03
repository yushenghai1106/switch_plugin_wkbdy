//! MCP（Model Context Protocol）stdio 服务器。
//!
//! 插件通过 `plugin.json` 的 `mcpServers` 启动 `wb-switch mcp`，客户端以
//! JSON-RPC 2.0 / 换行分隔（NDJSON）的方式在 stdin/stdout 上与本进程对话。
//!
//! 为什么独立于 HTTP 层：webui 形态靠 `wb-switch serve` + 浏览器，而插件形态下
//! 由宿主直接拉起本进程，走 stdio 不占端口、生命周期随宿主结束。两者共用
//! `wb-switch-core`，因此工具实现只是薄封装（与 `api.rs` 的路由一一对应）。
//!
//! stdout 是**协议通道**，任何日志都必须写 stderr —— 否则会污染 JSON-RPC 流。

use std::io::{BufRead, Write};
use std::time::Duration;

use serde_json::{json, Value};

use wb_switch_core::modules::{
    account, active_session, auth_file, checkin, codebuddy_cli, codebuddy_cn_ide, config,
    credit_usage, credits, daemon, jetbrains, limits, rotate, session, switch, token_stats, travel,
    update, variant::WbVariant, vscode_ext,
};

use crate::api::{cached_workbuddy_running, checkin_status_item};

/// 未协商出共同版本时使用的协议版本。
const DEFAULT_PROTOCOL_VERSION: &str = "2024-11-05";

/// 明确支持的协议版本；客户端请求命中其一则原样回显（规范要求）。
const SUPPORTED_PROTOCOL_VERSIONS: [&str; 3] = ["2024-11-05", "2025-03-26", "2025-06-18"];

const SERVER_NAME: &str = "wb-switch";

// ---------------------------------------------------------------------------
// 入参读取
// ---------------------------------------------------------------------------

/// 从工具入参解析档位（缺省国内版），与 Tauri 命令 / HTTP query 同义。
fn arg_variant(args: &Value) -> WbVariant {
    WbVariant::parse(args.get("variant").and_then(Value::as_str))
}

fn arg_str<'a>(args: &'a Value, key: &str) -> &'a str {
    args.get(key).and_then(Value::as_str).unwrap_or("")
}

fn arg_bool(args: &Value, key: &str, default: bool) -> bool {
    args.get(key).and_then(Value::as_bool).unwrap_or(default)
}

fn arg_string_list(args: &Value, key: &str) -> Vec<String> {
    args.get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// 服务循环
// ---------------------------------------------------------------------------

/// 启动 stdio 服务循环，直到 stdin 结束（宿主退出）。
///
/// 循环本身是**阻塞读**：MCP 是请求/响应式协议，逐行读最直接；阻塞的只是当前
/// 工作线程，异步的核心调用仍在运行时的其它线程上跑。
pub async fn run() {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    // 显式绑定锁：`stdin.lock().lines()` 依赖 for 循环的临时值延长规则，绑定更清晰也更稳。
    let input = stdin.lock();

    for line in input.lines() {
        let line = match line {
            Ok(line) => line,
            // stdin 读失败：宿主多半已经走了，直接收摊。
            Err(_) => break,
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        // 写不回宿主（stdout 管道已关）说明对方已经走了：继续读 stdin 只会让进程空转，
        // 这里直接收摊——写失败不再被静默忽略。
        let wrote = match serde_json::from_str::<Value>(trimmed) {
            Ok(message) => handle_message(&message, &mut out).await.is_ok(),
            Err(error) => {
                // 解析失败时拿不到 id，按 JSON-RPC 规定回 id: null。
                write_message(
                    &mut out,
                    &json!({
                        "jsonrpc": "2.0",
                        "id": Value::Null,
                        "error": { "code": -32700, "message": format!("JSON 解析失败: {error}") }
                    }),
                )
                .is_ok()
            }
        };
        if !wrote {
            break;
        }
    }
}

/// 分发单条 JSON-RPC 消息。
///
/// 通知（没有 `id` 字段）不得回包——回了会被宿主判为协议错误。
async fn handle_message(message: &Value, out: &mut impl Write) -> std::io::Result<()> {
    let method = message.get("method").and_then(Value::as_str).unwrap_or("");
    let id = message.get("id");

    // 通知：无论认不认识都静默吞掉。
    if id.is_none() {
        return Ok(());
    }
    let id = id.cloned().unwrap_or(Value::Null);
    let params = message.get("params").cloned().unwrap_or_else(|| json!({}));

    let result = match method {
        "initialize" => Ok(initialize_result(&params)),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tool_definitions() })),
        "tools/call" => call_tool(&params).await,
        _ => Err((-32601, format!("不支持的方法: {method}"))),
    };

    let envelope = match result {
        Ok(value) => json!({ "jsonrpc": "2.0", "id": id, "result": value }),
        Err((code, message)) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": code, "message": message }
        }),
    };
    write_message(out, &envelope)
}

/// 协商协议版本并声明能力。
fn initialize_result(params: &Value) -> Value {
    let requested = params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .unwrap_or("");
    let protocol_version = if SUPPORTED_PROTOCOL_VERSIONS.contains(&requested) {
        requested
    } else {
        DEFAULT_PROTOCOL_VERSION
    };

    json!({
        "protocolVersion": protocol_version,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": SERVER_NAME, "version": update::APP_VERSION },
    })
}

// ---------------------------------------------------------------------------
// 工具清单
// ---------------------------------------------------------------------------

/// 简单对象 schema 的快捷构造：`properties` 传入各字段定义。
fn object_schema(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

/// 工具清单。新增工具时同步更新此函数与 `call_tool`。
fn tool_definitions() -> Value {
    json!([
        {
            "name": "wb_status",
            "description": "查询本机 WorkBuddy 当前登录账号、认证文件路径与客户端运行状态。",
            "inputSchema": object_schema(json!({}), &[])
        },
        {
            "name": "wb_list_accounts",
            "description": "列出账号库里的全部账号（含是否为当前登录账号）。切换账号前先用它拿 account_id。",
            "inputSchema": object_schema(json!({}), &[])
        },
        {
            "name": "wb_client_status",
            "description": "查询某个客户端端点的登录状态：workbuddy / codebuddy-cli / codebuddy-ide / vscode-ext / jetbrains。",
            "inputSchema": object_schema(
                json!({
                    "client": {
                        "type": "string",
                        "enum": ["workbuddy", "codebuddy-cli", "codebuddy-ide", "vscode-ext", "jetbrains"],
                        "description": "要查询的客户端端点"
                    }
                }),
                &["client"]
            )
        },
        {
            "name": "wb_list_sessions",
            "description": "列出当前登录账号的会话（对话）。用于挑选要复制给其它账号的会话。",
            "inputSchema": object_schema(json!({}), &[])
        },
        {
            "name": "wb_switch_account",
            "description": "把 WorkBuddy 切换到指定账号（会先关闭再按需重开客户端）。仅影响 WorkBuddy 主客户端；切换 CodeBuddy CLI / IDE / 编辑器插件请用 wb_switch_client。",
            "inputSchema": object_schema(
                json!({
                    "account_id": { "type": "string", "description": "目标账号 id（来自 wb_list_accounts）" },
                    "restart": { "type": "boolean", "description": "切换后是否自动重开 WorkBuddy，默认 true" },
                    "copy_session_ids": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "可选：切换时顺带复制给目标账号的会话 id 列表"
                    }
                }),
                &["account_id"]
            )
        },
        {
            "name": "wb_switch_client",
            "description": "切换某个客户端端点的登录账号：codebuddy-cli / codebuddy-ide / vscode-ext / jetbrains。切换 WorkBuddy 请用 wb_switch_account。",
            "inputSchema": object_schema(
                json!({
                    "client": {
                        "type": "string",
                        "enum": ["codebuddy-cli", "codebuddy-ide", "vscode-ext", "jetbrains"],
                        "description": "要切换的客户端端点"
                    },
                    "account_id": { "type": "string", "description": "目标账号 id" },
                    "restart": { "type": "boolean", "description": "切换后是否自动重开客户端，默认 true" }
                }),
                &["client", "account_id"]
            )
        },
        {
            "name": "wb_copy_sessions",
            "description": "把当前账号的指定会话复制给目标账号：源账号数据不变，副本改新 id 写入目标账号，切换过去即可看到。",
            "inputSchema": object_schema(
                json!({
                    "target_account_id": { "type": "string", "description": "接收会话的目标账号 id" },
                    "session_ids": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "要复制的会话 id 列表（来自 wb_list_sessions）"
                    }
                }),
                &["target_account_id", "session_ids"]
            )
        },
        {
            "name": "wb_export_current_conversation",
            "description": "把**当前对话**导给另一个账号：副本以新 id 写入目标账号的会话库，源账号数据不变。client=workbuddy（默认）走 WorkBuddy；client=vscode-ext 走 VS Code 里的 CodeBuddy 插件（取该插件最近的对话）。WorkBuddy 正在运行时只能用 switch=true（切换流程会先关闭 WorkBuddy、再写入并重开）。",
            "inputSchema": object_schema(
                json!({
                    "target_account_id": { "type": "string", "description": "接收这段对话的目标账号 id" },
                    "client": {
                        "type": "string",
                        "enum": ["workbuddy", "vscode-ext"],
                        "description": "从哪个客户端取「当前对话」，默认 workbuddy"
                    },
                    "switch": {
                        "type": "boolean",
                        "description": "仅 workbuddy：是否顺带把 WorkBuddy 切换到目标账号，默认 false；WorkBuddy 运行中必须为 true"
                    }
                }),
                &["target_account_id"]
            )
        },
        {
            "name": "wb_checkin_status",
            "description": "查询签到状态。传 account_id 只查该账号，不传则返回全部账号。",
            "inputSchema": object_schema(
                json!({ "account_id": { "type": "string", "description": "可选：只查这个账号" } }),
                &[]
            )
        },
        {
            "name": "wb_checkin",
            "description": "签到。不传 account_id 时对所有账号执行签到。",
            "inputSchema": object_schema(
                json!({
                    "account_id": { "type": "string", "description": "可选：只签到这个账号" }
                }),
                &[]
            )
        },
        {
            "name": "wb_credit_expiry",
            "description": "查询某个账号的积分剩余量与到期时间（7 天内到期会标注紧迫）。",
            "inputSchema": object_schema(
                json!({ "account_id": { "type": "string", "description": "目标账号 id" } }),
                &["account_id"]
            )
        },
        {
            "name": "wb_credit_stats",
            "description": "积分用量统计：总览、近 30 天趋势、模型分类与账号消耗。",
            "inputSchema": object_schema(
                json!({ "refresh": { "type": "boolean", "description": "是否强制刷新缓存，默认 false" } }),
                &[]
            )
        },
        {
            "name": "wb_token_stats",
            "description": "Token 用量统计：总览、趋势、构成占比、活跃热力图与项目/模型排行。",
            "inputSchema": object_schema(
                json!({ "days": { "type": "integer", "description": "统计最近多少天，缺省由 core 决定" } }),
                &[]
            )
        },
        {
            "name": "wb_rate_limits",
            "description": "模型限额台账：各账号当前受限的模型与官方恢复时刻。",
            "inputSchema": object_schema(json!({}), &[])
        },
        {
            "name": "wb_travel_status",
            "description": "派猫猫旅行的状态：每个账号是否已派发、可领取情况。",
            "inputSchema": object_schema(json!({}), &[])
        },
        {
            "name": "wb_rotate_status",
            "description": "查询 CodeBuddy CLI 自动轮换的当前状态与配置。",
            "inputSchema": object_schema(json!({}), &[])
        },
        {
            "name": "wb_rotate_run",
            "description": "立即执行一次自动轮换：把积分最紧迫的账号设为 CodeBuddy CLI 的后续启动账号。",
            "inputSchema": object_schema(json!({}), &[])
        },
        {
            "name": "wb_daemon",
            "description": "查看、停止或开关后台周期任务（签到 / 旅行 / 自动轮换 / 保活 / 限额 hook 监听）。这些任务由随客户端自动拉起、且独立于客户端存活的守护进程执行；`disable` 会持久关掉它们（含不再自动拉起），只影响周期任务，账号切换与查询等按需能力不受影响。",
            "inputSchema": object_schema(
                json!({
                    "action": {
                        "type": "string",
                        "enum": ["status", "stop", "enable", "disable"],
                        "description": "status = 查询运行状态与总开关；stop = 只结束当前守护进程（下次会话还会被拉起）；enable / disable = 持久开关后台周期任务"
                    }
                }),
                &["action"]
            )
        },
        {
            "name": "wb_open_webui",
            "description": "打开本地 Web 界面（账号管理、积分与 Token 统计图表等完整界面）。已在运行则直接打开浏览器，否则按需拉起服务再打开。",
            "inputSchema": object_schema(
                json!({
                    "port": {
                        "type": "integer",
                        "description": "本地服务端口，默认 57890"
                    }
                }),
                &[]
            )
        }
    ])
}

// ---------------------------------------------------------------------------
// 工具执行
// ---------------------------------------------------------------------------

/// 成功结果：把结构化数据以 pretty JSON 交给模型渲染。
fn ok_content(value: &Value) -> Value {
    json!({
        "content": [{ "type": "text", "text": to_pretty(value) }],
        "isError": false
    })
}

/// 失败结果：按 MCP 约定回 `isError: true` 的**内容**而非 JSON-RPC error ——
/// 前者模型能读到原因并自行纠正，后者会被宿主当协议故障。
fn err_content(message: impl Into<String>) -> Value {
    json!({
        "content": [{ "type": "text", "text": message.into() }],
        "isError": true
    })
}

/// 把 `Result<Value, String>` 收成工具结果。
fn from_result(result: Result<Value, String>) -> Value {
    match result {
        Ok(value) => ok_content(&value),
        Err(error) => err_content(error),
    }
}

/// 执行 `tools/call`。
async fn call_tool(params: &Value) -> Result<Value, (i32, String)> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| (-32602, "tools/call 缺少 name".to_string()))?;
    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));

    // 未知工具名属于**协议层**错误（宿主传了 schema 里不存在的名字），与下面各工具
    // 返回的「业务失败」（isError 内容）区分开。判据直接取工具清单，避免两处漂移。
    if !declares_tool(name) {
        return Err((-32602, format!("未知工具: {name}")));
    }

    let result = match name {
        "wb_status" => ok_content(&status_payload(arg_variant(&args))),
        "wb_list_accounts" => ok_content(&accounts_payload(arg_variant(&args))),
        "wb_client_status" => client_status(&args),
        "wb_list_sessions" => ok_content(&sessions_payload(arg_variant(&args))),
        "wb_switch_account" => switch_workbuddy(&args),
        "wb_switch_client" => switch_client(&args),
        "wb_copy_sessions" => copy_sessions(&args),
        "wb_export_current_conversation" => export_current_conversation(&args),
        "wb_checkin_status" => checkin_status(&args).await,
        "wb_checkin" => run_checkin(&args).await,
        "wb_credit_expiry" => credit_expiry(&args).await,
        "wb_credit_stats" => {
            ok_content(&credit_usage::get_statistics(arg_bool(&args, "refresh", false)).await)
        }
        "wb_token_stats" => {
            let days = args.get("days").and_then(Value::as_i64);
            ok_content(&token_stats::get_statistics(days))
        }
        "wb_rate_limits" => ok_content(&limits::get_rate_limits()),
        "wb_travel_status" => travel_status().await,
        "wb_rotate_status" => ok_content(&rotate::rotate_status()),
        "wb_rotate_run" => ok_content(&rotate::run_rotate_cycle().await),
        "wb_daemon" => daemon_tool(&args),
        "wb_open_webui" => open_webui(&args),
        // 清单里有、实现没跟上：回可读错误而不是 panic，保证宿主不被打断。
        other => err_content(format!("工具已声明但未实现: {other}")),
    };
    Ok(result)
}

/// 工具名是否已在清单里声明（`call_tool` 的前置校验）。
fn declares_tool(name: &str) -> bool {
    tool_definitions().as_array().is_some_and(|tools| {
        tools
            .iter()
            .any(|tool| tool.get("name").and_then(Value::as_str) == Some(name))
    })
}

/// `wb_status` 的数据体（与 `api.rs` 的 `GET /api/status` 同源）。
fn status_payload(variant: WbVariant) -> Value {
    let auth = auth_file::read_auth_file(variant);
    let current = auth.as_ref().map(|a| {
        let acct = a.get("account").cloned().unwrap_or_else(|| json!({}));
        json!({
            "uid": account::display_value(&acct, "uid"),
            "nickname": account::display_value(&acct, "nickname"),
            "email": account::display_value(&acct, "email"),
        })
    });

    json!({
        "running": cached_workbuddy_running(variant),
        "authFile": auth_file::auth_file_path(variant).to_string_lossy(),
        "current": current,
        "appPath": auth_file::workbuddy_app_path(variant).to_string_lossy(),
        "version": update::APP_VERSION,
        "variant": variant.as_str(),
    })
}

/// `wb_list_accounts` 的数据体（与 `GET /api/accounts` 同源）。
fn accounts_payload(variant: WbVariant) -> Value {
    json!({
        "accounts": account::load_accounts()
            .iter()
            .map(account::account_meta)
            .collect::<Vec<_>>(),
        "current": auth_file::read_auth_file(variant)
            .and_then(|a| a.get("account").and_then(|x| x.get("uid")).and_then(|x| x.as_str()).map(String::from)),
        "variant": variant.as_str(),
    })
}

/// `wb_list_sessions` 的数据体（与 `GET /api/sessions` 同源）。
fn sessions_payload(variant: WbVariant) -> Value {
    match session::current_user_uid(variant) {
        Some(uid) => json!({
            "sessions": session::list_sessions_for_user(variant, &uid),
            "current": uid,
            "variant": variant.as_str(),
        }),
        None => json!({ "sessions": [], "current": Value::Null, "variant": variant.as_str() }),
    }
}

/// `wb_client_status`：按客户端端点分派到各自的 status。
fn client_status(args: &Value) -> Value {
    let variant = arg_variant(args);
    match arg_str(args, "client") {
        "workbuddy" => ok_content(&status_payload(variant)),
        "codebuddy-cli" => ok_content(&codebuddy_cli::status()),
        "codebuddy-ide" => ok_content(&codebuddy_cn_ide::status()),
        "vscode-ext" => ok_content(&vscode_ext::status()),
        "jetbrains" => ok_content(&jetbrains::status()),
        other => err_content(format!("未知客户端端点: {other}")),
    }
}

/// `wb_switch_account`：切 WorkBuddy 主客户端。
///
/// `switch_account` 是同步阻塞调用（内部要关进程、写认证、再拉起），在异步上下文里
/// 直接调用会占住一个工作线程——MCP 工具调用本身是串行的，这里可以接受，也省掉了
/// 把一堆引用搬进 `spawn_blocking` 的复杂度。
fn switch_workbuddy(args: &Value) -> Value {
    let account_id = arg_str(args, "account_id");
    if account_id.trim().is_empty() {
        return err_content("缺少 account_id");
    }
    let restart = arg_bool(args, "restart", true);
    let copy_ids = arg_string_list(args, "copy_session_ids");
    from_result(switch::switch_account(
        None,
        account_id,
        restart,
        false,
        &copy_ids,
        &[],
    ))
}

/// `wb_switch_client`：切 CodeBuddy CLI / IDE / 编辑器插件端点。
///
/// 各端点由 core 各自负责关进程与重开，这里只做参数转发。这些调用同样是同步阻塞的
/// （CodeBuddy CLI 会先关掉正在跑的 CLI），与 `switch_workbuddy` 同一取舍。
fn switch_client(args: &Value) -> Value {
    let account_id = arg_str(args, "account_id");
    if account_id.trim().is_empty() {
        return err_content("缺少 account_id");
    }
    let restart = arg_bool(args, "restart", true);
    match arg_str(args, "client") {
        "codebuddy-cli" => from_result(codebuddy_cli::switch_active_account(account_id)),
        "codebuddy-ide" => from_result(codebuddy_cn_ide::switch_account(account_id, restart)),
        "vscode-ext" => from_result(vscode_ext::switch_account(account_id, restart)),
        // 缺省 config_dirs = 全部装了插件的 IDE，与 /api/jetbrains/switch 同义。
        "jetbrains" => from_result(jetbrains::switch_account(account_id, restart, None)),
        other => err_content(format!("不支持切换的客户端端点: {other}")),
    }
}

/// `wb_copy_sessions`：与 `POST /api/sessions/copy` 同源。
fn copy_sessions(args: &Value) -> Value {
    let target_id = arg_str(args, "target_account_id");
    if target_id.trim().is_empty() {
        return err_content("缺少 target_account_id");
    }
    let session_ids = arg_string_list(args, "session_ids");
    if session_ids.is_empty() {
        return err_content("缺少 session_ids（要复制的会话 id 列表）");
    }
    let Some(target) = account::find_account(target_id) else {
        return err_content("目标账号不存在");
    };
    let variant = account::variant_of(&target);
    match session::copy_sessions_for_switch(&target, &session_ids) {
        Ok(mut report) => {
            report["variant"] = json!(variant.as_str());
            ok_content(&report)
        }
        Err(error) => err_content(error),
    }
}

/// `wb_export_current_conversation`：把当前对话导给目标账号。
///
/// 与 `wb_copy_sessions` 的区别只在「复制哪个会话」：这里由 core 的 active_session
/// 指针（hook 记录）解析，解析不到时回退到最近一条带正文的会话；`switch=true` 时
/// 走切换流程，因此可以边跑 WorkBuddy 边导出。
fn export_current_conversation(args: &Value) -> Value {
    let target_id = arg_str(args, "target_account_id");
    if target_id.trim().is_empty() {
        return err_content("缺少 target_account_id");
    }

    // 先校验入口参数，再查账号：参数本身就错时不该拿「账号不存在」当答复。
    // 抽成纯函数也是为了让它可测——否则这两条错误路径只有在本机有账号时才走得到。
    let client =
        match resolve_export_client(arg_str(args, "client"), arg_bool(args, "switch", false)) {
            Ok(client) => client,
            Err(error) => return err_content(error),
        };

    let Some(target) = account::find_account(target_id) else {
        return err_content("目标账号不存在");
    };

    match client {
        ExportClient::Workbuddy => from_result(active_session::export_to_account(
            &target,
            arg_bool(args, "switch", false),
        )),
        ExportClient::VscodeExt => from_result(active_session::export_vscode_conversation(&target)),
    }
}

/// 导出的目标端。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExportClient {
    /// WorkBuddy 主客户端（按 hook 指针 / 最近会话确定「当前对话」）。
    Workbuddy,
    /// VS Code 里的 CodeBuddy 插件（按该插件最近的对话确定）。
    VscodeExt,
}

/// 解析并校验导出目标端。
///
/// `switch` 只对 WorkBuddy 有意义：VS Code 插件端没有「导出并切换」这条组合流程，
/// 静默忽略会让用户以为账号也切过去了，所以明确拒绝。
fn resolve_export_client(client: &str, switch: bool) -> Result<ExportClient, String> {
    match client {
        "" | "workbuddy" => Ok(ExportClient::Workbuddy),
        "vscode-ext" => {
            if switch {
                return Err(
                    "VS Code 插件端不支持「导出并切换」；请把 switch 设为 false，\
                     并先完全退出 VS Code 再导出。"
                        .to_string(),
                );
            }
            Ok(ExportClient::VscodeExt)
        }
        other => Err(format!(
            "未知 client: {other}（可用 workbuddy / vscode-ext）"
        )),
    }
}

/// `wb_daemon`：查询 / 停止后台周期任务。
///
/// 判据始终是**锁**而不是 PID 文件：PID 文件只是诊断信息，进程被强杀时不会清理，
/// 单独拿它当判据会把已死进程当成在运行（`daemon::running_pid` 内部已经这么处理）。
fn daemon_tool(args: &Value) -> Value {
    match arg_str(args, "action") {
        "status" => {
            let pid = daemon::running_pid();
            ok_content(&json!({
                "running": pid.is_some(),
                "pid": pid,
                "backgroundTasksEnabled": config::background_tasks_enabled(),
                "configFile": config::daemon_config_file().to_string_lossy(),
            }))
        }
        "stop" => match daemon::stop_running() {
            Ok(message) => ok_content(&json!({ "stopped": true, "message": message })),
            Err(error) => err_content(error),
        },
        // enable / disable 改的是**持久配置**：只 stop 的话下次会话又会被拉起来，
        // 想彻底关掉必须有这一层，否则「关闭」只是暂时生效。
        "enable" | "disable" => {
            let enabled = arg_str(args, "action") == "enable";
            if let Err(error) = config::save_daemon_config(&json!({ "backgroundTasks": enabled })) {
                return err_content(format!("保存后台任务配置失败：{error}"));
            }
            // 关闭时顺带结束正在跑的守护：否则「已经关了但进程还在跑」看起来像没生效。
            // 开启时不动进程——下次会话启动（或 MCP 宿主重启）自然会拉起。
            let stopped = if enabled {
                None
            } else {
                daemon::stop_running().ok()
            };
            ok_content(&json!({
                "backgroundTasksEnabled": enabled,
                "stopped": stopped,
                "configFile": config::daemon_config_file().to_string_lossy(),
            }))
        }
        other => err_content(format!(
            "未知 action: {other}（可用 status / stop / enable / disable）"
        )),
    }
}

/// `wb_open_webui`：按需拉起本地 Web 界面并打开浏览器。
///
/// 「按需」的含义：已经在服务就直接开浏览器。重复 bind 必然失败，而且白起一个进程
/// 没有意义（它还会去抢后台任务的执行权——虽然抢不到，但没必要）。
fn open_webui(args: &Value) -> Value {
    let port = match args.get("port").and_then(Value::as_u64) {
        Some(raw) if (1..=65535).contains(&raw) => raw as u16,
        Some(raw) => return err_content(format!("端口不合法: {raw}")),
        None => 57890,
    };
    let addr = format!("127.0.0.1:{port}");

    let already_running = probe_local_service(&addr);
    if !already_running {
        if let Err(error) = spawn_local_service(port) {
            return err_content(error);
        }
        // 等它就绪再开浏览器：否则用户会先看到一个「无法访问」的页面。
        if !wait_for_service(&addr, Duration::from_secs(8)) {
            return err_content(format!(
                "本地服务未能在预期时间内就绪（{addr}）。可手动运行 `wb-switch serve` 查看报错。"
            ));
        }
    }

    // 带上访问令牌再开：否则用户先看到一个「缺少访问令牌」的界面。
    // 服务是旧版（无令牌文件）时退回不带令牌的链接。
    let url = match config::load_webui_token(port) {
        Some(token) => format!("http://{addr}/?token={token}"),
        None => format!("http://{addr}"),
    };
    crate::open_browser_url(&url);
    ok_content(&json!({
        "url": url,
        "alreadyRunning": already_running,
    }))
}

/// 探测 `addr` 上是否已有**本工具**的服务在响应。
///
/// 不只做 TCP 握手：那个端口可能被别的程序占着，只握手会误判——然后给用户打开一个
/// 不相干的页面，还报「成功」。这里发一次最小 HTTP 请求打 `/api/status`，要求响应里
/// 带我们的版本字段，足以把「我们的服务」与「其它占用者」区分开。
fn probe_local_service(addr: &str) -> bool {
    use std::io::{Read, Write};

    let Ok(socket) = addr.parse() else {
        return false;
    };
    let Ok(mut stream) = std::net::TcpStream::connect_timeout(&socket, Duration::from_millis(500))
    else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(800)));
    // 服务要求访问令牌（见 `api::require_webui_token`）。令牌按端口落在
    // `~/.wb-switch/webui.token.<port>`，这里读同一份带上；读不到就按无令牌探测，
    // 以便仍能识别旧版内核或未启用校验的服务。
    let token_header = addr
        .rsplit(':')
        .next()
        .and_then(|port| port.parse::<u16>().ok())
        .and_then(config::load_webui_token)
        .map(|token| format!("X-WB-Token: {token}\r\n"))
        .unwrap_or_default();
    let request = format!("GET /api/status HTTP/1.0\r\nHost: {addr}\r\n{token_header}\r\n");
    if stream.write_all(request.as_bytes()).is_err() {
        return false;
    }
    let mut buffer = [0u8; 1024];
    let Ok(read) = stream.read(&mut buffer) else {
        return false;
    };
    let text = String::from_utf8_lossy(&buffer[..read]);
    text.starts_with("HTTP/1.") && text.contains("\"version\"")
}

/// 轮询等待服务就绪。
fn wait_for_service(addr: &str, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if probe_local_service(addr) {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// 以**分离进程**拉起 `serve --no-open`（浏览器由调用方自己开，避免开两次）。
fn spawn_local_service(port: u16) -> Result<(), String> {
    let exe =
        std::env::current_exe().map_err(|error| format!("无法定位自身可执行文件：{error}"))?;
    let mut command = std::process::Command::new(exe);
    command.args(["serve", "--port", &port.to_string(), "--no-open"]);
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW：拉一个后台服务不该闪出控制台黑框。
        command.creation_flags(0x0800_0000);
    }

    command
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("启动本地服务失败：{error}"))
}

/// `wb_checkin_status`：传 account_id 只查该账号，否则批量。
async fn checkin_status(args: &Value) -> Value {
    let account_id = arg_str(args, "account_id");
    if !account_id.trim().is_empty() {
        let Some(acc) = account::find_account(account_id) else {
            return err_content("账号不存在");
        };
        let status = checkin::get_checkin_status_for_display(&acc).await;
        return ok_content(&checkin_status_item(&acc, status));
    }

    let mut items = Vec::new();
    for acc in &account::load_accounts() {
        let status = checkin::get_checkin_status_for_display(acc).await;
        items.push(checkin_status_item(acc, status));
    }
    ok_content(&json!({ "accounts": items }))
}

/// `wb_checkin`：传 account_id 签单个，否则按档位批量签。
async fn run_checkin(args: &Value) -> Value {
    let account_id = arg_str(args, "account_id");
    if !account_id.trim().is_empty() {
        let Some(acc) = account::find_account(account_id) else {
            return err_content("账号不存在");
        };
        return ok_content(&checkin::checkin_account(&acc).await);
    }
    // 未指定档位 = 全部档位，与 `/api/checkin/all` 缺省行为一致。
    let variant = args
        .get("variant")
        .and_then(Value::as_str)
        .map(|raw| WbVariant::parse(Some(raw)));
    ok_content(&checkin::run_checkin_all(variant).await)
}

/// `wb_credit_expiry`：与 `POST /api/credits` 同源。
async fn credit_expiry(args: &Value) -> Value {
    let account_id = arg_str(args, "account_id");
    let Some(acc) = account::find_account(account_id) else {
        return err_content("账号不存在");
    };
    ok_content(&credits::get_credit_expiry(&acc).await)
}

/// `wb_travel_status`：与 `GET /api/travel/status` 同源。
async fn travel_status() -> Value {
    travel::reconcile_due_travel(None).await;
    let items = account::load_accounts()
        .iter()
        .map(|acc| {
            let id = acc.get("id").and_then(Value::as_str).unwrap_or("");
            let mut value = travel::travel_display(id);
            value["accountId"] = acc.get("id").cloned().unwrap_or(Value::Null);
            value["email"] = json!(account::account_display_name(acc));
            value
        })
        .collect::<Vec<_>>();
    ok_content(&json!({ "accounts": items }))
}

fn to_pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

/// 写出一条 NDJSON 消息并立即 flush：宿主按行读取，攒着不发会死等。
fn write_message(out: &mut impl Write, message: &Value) -> std::io::Result<()> {
    let line = serde_json::to_string(message).unwrap_or_else(|_| "{}".to_string());
    writeln!(out, "{line}")?;
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 客户端请求受支持的版本时必须原样回显，否则宿主可能拒绝握手。
    #[test]
    fn initialize_echoes_supported_version() {
        let params = json!({ "protocolVersion": "2025-06-18" });
        assert_eq!(
            initialize_result(&params)
                .get("protocolVersion")
                .and_then(Value::as_str),
            Some("2025-06-18")
        );
    }

    /// 不认识的版本回退到默认版本，且不 panic。
    #[test]
    fn initialize_falls_back_on_unknown_version() {
        let params = json!({ "protocolVersion": "1999-01-01" });
        assert_eq!(
            initialize_result(&params)
                .get("protocolVersion")
                .and_then(Value::as_str),
            Some(DEFAULT_PROTOCOL_VERSION)
        );
    }

    #[test]
    fn initialize_tolerates_missing_version() {
        assert_eq!(
            initialize_result(&json!({}))
                .get("protocolVersion")
                .and_then(Value::as_str),
            Some(DEFAULT_PROTOCOL_VERSION)
        );
    }

    /// 每个工具都必须有 name / description / inputSchema，否则宿主会加载失败。
    #[test]
    fn tool_definitions_are_well_formed() {
        let tools = tool_definitions();
        let list = tools.as_array().expect("工具清单应为数组");
        assert!(!list.is_empty());
        let mut names = Vec::new();
        for tool in list {
            let name = tool.get("name").and_then(Value::as_str).unwrap_or("");
            assert!(!name.is_empty(), "工具缺少 name");
            assert!(tool.get("description").is_some(), "{name} 缺少 description");
            let schema = tool.get("inputSchema").expect("缺少 inputSchema");
            assert_eq!(schema.get("type").and_then(Value::as_str), Some("object"));
            names.push(name.to_string());
        }
        // 重名会让宿主后加载的覆盖前者，必须拦住。
        let mut unique = names.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), names.len(), "存在重名工具: {names:?}");
    }

    /// 声明为必填的字段必须真在 properties 里，否则宿主校验会与描述不一致。
    #[test]
    fn required_fields_exist_in_properties() {
        for tool in tool_definitions().as_array().expect("应为数组") {
            let name = tool.get("name").and_then(Value::as_str).unwrap_or("");
            let schema = tool.get("inputSchema").expect("缺少 inputSchema");
            let properties = schema.get("properties").expect("缺少 properties");
            for field in schema
                .get("required")
                .and_then(Value::as_array)
                .map(|v| v.as_slice())
                .unwrap_or(&[])
            {
                let key = field.as_str().unwrap_or("");
                assert!(
                    properties.get(key).is_some(),
                    "{name} 的必填字段 {key} 未在 properties 中声明"
                );
            }
        }
    }

    /// 通知没有 id，必须完全静默（回包会被宿主判为协议错误）。
    #[tokio::test]
    async fn notifications_get_no_response() {
        let mut out: Vec<u8> = Vec::new();
        let _ = handle_message(
            &json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
            &mut out,
        )
        .await;
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn unknown_method_reports_method_not_found() {
        let mut out: Vec<u8> = Vec::new();
        let _ = handle_message(
            &json!({ "jsonrpc": "2.0", "id": 1, "method": "nope/nope" }),
            &mut out,
        )
        .await;
        let text = String::from_utf8(out).expect("应输出 UTF-8");
        let parsed: Value = serde_json::from_str(text.trim()).expect("应是合法 JSON");
        assert_eq!(
            parsed.pointer("/error/code").and_then(Value::as_i64),
            Some(-32601)
        );
    }

    /// 未知工具名走工具结果（`isError`），不是 JSON-RPC error。
    #[tokio::test]
    async fn unknown_tool_reports_protocol_error() {
        let mut out: Vec<u8> = Vec::new();
        let _ = handle_message(
            &json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": "nope" } }),
            &mut out,
        )
        .await;
        let text = String::from_utf8(out).expect("应输出 UTF-8");
        let parsed: Value = serde_json::from_str(text.trim()).expect("应是合法 JSON");
        assert_eq!(
            parsed.pointer("/error/code").and_then(Value::as_i64),
            Some(-32602)
        );
    }

    /// 业务失败（如目标账号不存在）必须回 `isError: true` 的内容，而不是 JSON-RPC error。
    #[tokio::test]
    async fn business_failure_uses_is_error_content() {
        let mut out: Vec<u8> = Vec::new();
        let _ = handle_message(
            &json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": { "name": "wb_copy_sessions", "arguments": { "target_account_id": "", "session_ids": [] } }
            }),
            &mut out,
        )
        .await;
        let text = String::from_utf8(out).expect("应输出 UTF-8");
        let parsed: Value = serde_json::from_str(text.trim()).expect("应是合法 JSON");
        assert!(parsed.get("error").is_none(), "不该是 JSON-RPC error");
        assert_eq!(
            parsed.pointer("/result/isError").and_then(Value::as_bool),
            Some(true)
        );
    }

    #[test]
    fn arg_readers_tolerate_missing_and_wrong_types() {
        let args = json!({ "s": "x", "b": true, "list": ["a", 1, "b"] });
        assert_eq!(arg_str(&args, "s"), "x");
        assert_eq!(arg_str(&args, "missing"), "");
        assert_eq!(arg_str(&args, "b"), "", "类型不符时按空串处理");
        assert!(arg_bool(&args, "b", false));
        assert!(!arg_bool(&args, "missing", false));
        assert_eq!(arg_string_list(&args, "list"), vec!["a", "b"]);
        assert!(arg_string_list(&args, "missing").is_empty());
    }

    /// 导出目标端的解析与校验。
    ///
    /// 这几条错误路径**必须**能独立测：真正的分派发生在账号查得到之后，而本机
    /// 账号库可能是空的——只靠端到端冒烟永远走不到这里。
    #[test]
    fn export_client_resolution() {
        assert_eq!(
            resolve_export_client("", false),
            Ok(ExportClient::Workbuddy)
        );
        assert_eq!(
            resolve_export_client("workbuddy", true),
            Ok(ExportClient::Workbuddy),
            "switch 对 WorkBuddy 合法"
        );
        assert_eq!(
            resolve_export_client("vscode-ext", false),
            Ok(ExportClient::VscodeExt)
        );

        // VS Code 端没有「导出并切换」：必须明确拒绝，静默忽略会让用户以为号也切了。
        let error = resolve_export_client("vscode-ext", true).expect_err("应拒绝 switch");
        assert!(error.contains("不支持「导出并切换」"), "{error}");
        assert!(error.contains("退出 VS Code"), "应给出可操作指引: {error}");

        let error = resolve_export_client("bogus", false).expect_err("应拒绝未知 client");
        assert!(error.contains("未知 client"), "{error}");
    }

    /// 起一个「假服务」：接受一次连接并回固定响应。
    fn serve_once(response: &'static [u8]) -> String {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("绑定随机端口");
        let addr = listener.local_addr().expect("取本地地址").to_string();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut request = [0u8; 512];
                let _ = stream.read(&mut request);
                let _ = stream.write_all(response);
                let _ = stream.flush();
            }
        });
        addr
    }

    /// 探测必须认得出「响应里带我们的版本字段」的服务。
    #[test]
    fn probe_accepts_our_own_service() {
        // 版本值刻意用中性数字：这里只验证「响应体含 version 字段」这一判据，
        // 写成真实版本号的话，将来 grep 版本时会误以为漏了 bump。
        let addr = serve_once(b"HTTP/1.0 200 OK\r\n\r\n{\"version\":\"1.2.3\"}");
        assert!(
            probe_local_service(&addr),
            "带 version 字段的响应应判为我们的服务"
        );
    }

    /// 关键区分：**端口被别的程序占着**时不能误判成我们的服务。
    ///
    /// 只做 TCP 握手就会误判——然后给用户打开一个不相干的页面还报「成功」。
    #[test]
    fn probe_rejects_foreign_http_service() {
        let addr = serve_once(b"HTTP/1.0 200 OK\r\n\r\n<h1>someone else</h1>");
        assert!(
            !probe_local_service(&addr),
            "不含 version 字段的响应不该被认成我们的服务"
        );
    }

    /// 端口上什么都没有时必须是 false（不能靠异常吞掉蒙对）。
    #[test]
    fn probe_reports_false_when_nothing_listens() {
        use std::net::TcpListener;
        // 绑一个随机端口拿到号后立刻释放：该端口随即无人监听。
        let listener = TcpListener::bind("127.0.0.1:0").expect("绑定随机端口");
        let addr = listener.local_addr().expect("取本地地址").to_string();
        drop(listener);
        assert!(!probe_local_service(&addr));
    }

    /// 非法端口必须在**做任何副作用之前**就被拒绝（此用例不会拉起服务、也不会开浏览器）。
    #[tokio::test]
    async fn open_webui_rejects_invalid_port() {
        let mut out: Vec<u8> = Vec::new();
        let _ = handle_message(
            &json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": { "name": "wb_open_webui", "arguments": { "port": 70000 } }
            }),
            &mut out,
        )
        .await;

        let text = String::from_utf8(out).expect("应输出 UTF-8");
        let parsed: Value = serde_json::from_str(text.trim()).expect("应是合法 JSON");
        assert_eq!(
            parsed.pointer("/result/isError").and_then(Value::as_bool),
            Some(true)
        );
        assert!(
            parsed
                .pointer("/result/content/0/text")
                .and_then(Value::as_str)
                .is_some_and(|s| s.contains("端口")),
            "错误信息应说明端口不合法"
        );
    }
}
