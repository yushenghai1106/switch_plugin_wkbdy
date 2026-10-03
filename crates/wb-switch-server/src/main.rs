//! workbuddy-switch CLI：npm 安装形态的入口。
//!
//! ```bash
//! workbuddy-switch              # 启动本地服务 + 打开浏览器 webui
//! workbuddy-switch serve        # 只起服务不开浏览器（--port / --no-open）
//! workbuddy-switch daemon       # 只跑后台周期任务（签到 / 旅行 / 轮换 / 保活 / 限额监听）
//! workbuddy-switch daemon --stop  # 结束正在运行的后台任务
//! workbuddy-switch mcp          # MCP stdio 服务器（由 CodeBuddy / WorkBuddy 插件拉起）
//! workbuddy-switch hook-record  # 从 stdin 读 hook payload，记录「当前对话」指针
//! workbuddy-switch status       # 终端输出当前账号
//! workbuddy-switch version      # 版本号
//! ```

mod api;
mod mcp;

use serde_json::json;

use wb_switch_core::modules::{
    account, active_session, auth_file, config, daemon, process, update, variant::WbVariant,
};

fn default_port() -> u16 {
    57890
}

/// 启动后台周期任务，并**按需**成为本机唯一的执行者。
///
/// `serve` / `daemon` / 桌面版三者的循环内容完全一致，同时跑会重复签到、重复轮换、
/// 重复派旅行。因此统一由 `daemon` 模块的跨进程锁选出一个执行者；没抢到锁的宿主
/// 只提供自己的服务能力。抢到后锁由 `daemon` 模块持有到进程结束，这里不需要保存它。
fn spawn_background_loops() {
    match daemon::start_if_elected(
        || {},
        |message| {
            // 无 UI 宿主：轮换推迟在终端记一笔即可。轮换日志里已有同样内容，
            // 这里只是让前台运行时看得见，不另做持久化。
            if let Some(body) = message.get("body").and_then(|v| v.as_str()) {
                eprintln!("[轮换] 已推迟：{body}");
            }
        },
    ) {
        Ok(()) => {}
        // 已有执行者：正常情况，不是错误。
        Err(daemon::DaemonLockError::Busy) => {
            eprintln!("[后台任务] 已有进程在运行，本进程只提供服务，不重复跑周期任务");
        }
        // 建立不了互斥时不跑循环：宁可这一轮不跑，也不要演变成多份并发循环。
        Err(error) => {
            eprintln!("[后台任务] {}，本进程不跑周期任务", error.message());
        }
    }
}

/// CLI 档位参数：`--variant ai` / `--variant=ai`；缺省国内版。
///
/// 与 Tauri 命令的可选 `variant` 参数、HTTP 路由的 query/body 字段同义。
fn variant_arg(args: &[String]) -> WbVariant {
    let raw = args.iter().enumerate().find_map(|(index, arg)| {
        if let Some(value) = arg.strip_prefix("--variant=") {
            return Some(value.to_string());
        }
        arg.eq("--variant")
            .then(|| args.get(index + 1).cloned().unwrap_or_default())
    });
    WbVariant::parse(raw.as_deref())
}

fn print_status(variant: WbVariant) {
    let auth = auth_file::read_auth_file(variant);
    let current = auth.as_ref().map(|a| {
        let acct = a.get("account").cloned().unwrap_or_else(|| json!({}));
        json!({
            "uid": account::display_value(&acct, "uid"),
            "nickname": account::display_value(&acct, "nickname"),
            "email": account::display_value(&acct, "email"),
        })
    });
    let running = process::is_workbuddy_running(variant);
    println!("workbuddy-switch v{}", update::APP_VERSION);
    println!("WorkBuddy 运行中: {}", if running { "是" } else { "否" });
    match current {
        Some(c) => {
            let name = c
                .get("nickname")
                .and_then(|v| v.as_str())
                .or_else(|| c.get("email").and_then(|v| v.as_str()))
                .unwrap_or("未知");
            println!("当前账号: {name}");
        }
        None => println!("当前账号: 未登录"),
    }
    println!("账号数: {}", account::load_accounts().len());
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let raw = args.get(1).map(|s| s.as_str()).unwrap_or("");

    // 把「选项」和「子命令」分开：既有用法允许省略 `serve` 直接给选项
    // （`workbuddy-switch --port 8080`），过去靠 `_ => serve` 兜底才成立。
    // 现在兜底分支改成报错，就必须在这里显式把这类调用归到 serve，
    // 否则改好「未知子命令」这个坑的同时会踩坏既有命令行用法。
    let cmd = match raw {
        "" => "serve",
        // 版本 / 帮助旗标既是子命令也是选项，先认掉，别被下面的 `-` 规则吞进 serve。
        "--version" | "-V" => "version",
        "-h" | "--help" => "help",
        // 选项而非子命令：`workbuddy-switch --port 8080` 等价于 `serve --port 8080`。
        // 注意：其它任何 `-` 开头的输入也会落到 serve（例如拼错的 `--prot 8080`），
        // 所以 `-h` / `--help` 必须在上面的分支里显式认掉，否则会静默起一个常驻服务、
        // 抢走默认端口、还顺手弹出浏览器——用户以为只是要看帮助。
        _ if raw.starts_with('-') => "serve",
        other => other,
    };

    match cmd {
        "status" => print_status(variant_arg(&args)),
        "version" => {
            println!("workbuddy-switch {}", env!("CARGO_PKG_VERSION"));
        }
        "help" => {
            println!("workbuddy-switch {}", env!("CARGO_PKG_VERSION"));
            println!("{}", usage_line());
        }
        // MCP stdio 服务器：插件经 plugin.json 的 mcpServers 拉起，阻塞跑到 stdin 结束。
        "mcp" => mcp::run().await,
        // 插件 hook 入口：把「当前是哪个对话」记下来，供导出工具读取。
        "hook-record" => hook_record(),
        // 只跑后台周期任务，不起 web 服务：插件在会话启动时以分离进程拉起它。
        "daemon" => run_daemon(&args).await,
        "serve" => serve(&args).await,
        // 未知子命令必须**报错退出**，不能兜底成 serve。
        //
        // 兜底成 serve 是个真实的坑：老版本内核被新插件以未知子命令拉起时，会静默启动
        // 一个常驻 web 服务器并一直阻塞——而插件 hook 是挂在每轮对话结束上的，客户端会
        // 一直卡到 hook 超时。实测在旧内核上跑 `hook-record` 就是这个结果。
        other => {
            eprintln!("未知子命令: {other}");
            eprintln!("{}", usage_line());
            std::process::exit(2);
        }
    }
}

/// 可用子命令文案：`-h` / `--help` 打到 stdout，未知子命令打到 stderr。
fn usage_line() -> &'static str {
    "可用子命令: serve [--port N] [--no-open] | daemon [--stop] | status | mcp | hook-record | version | help"
}

/// `daemon`：只跑后台周期任务，阻塞到进程被终止；`--stop` 则结束正在运行的守护。
///
/// 插件在每次会话启动时都会尝试拉起它（幂等）：抢不到锁说明已有执行者，本次**安静
/// 退出并返回 0**——那是正常情况，不该让插件把 hook 判成失败。
async fn run_daemon(args: &[String]) {
    // `--stop`：优先于启动。用户要的是「把后台任务停下来」。
    if args.iter().any(|arg| arg == "--stop") {
        match daemon::stop_running() {
            Ok(message) => println!("{message}"),
            Err(error) => {
                eprintln!("[daemon] {error}");
                std::process::exit(1);
            }
        }
        return;
    }

    match daemon::start_if_elected(
        || {},
        |message| {
            if let Some(body) = message.get("body").and_then(|v| v.as_str()) {
                eprintln!("[轮换] 已推迟：{body}");
            }
        },
    ) {
        Ok(()) => {}
        Err(daemon::DaemonLockError::Busy) => {
            eprintln!("[daemon] 已有进程在运行后台任务，本次退出");
            return;
        }
        // 配置里关掉了后台周期任务：这不是失败，安静退出（退出码 0），
        // 否则插件每次会话启动都会在 daemon.log 里记一条误导性的错误。
        Err(daemon::DaemonLockError::Disabled) => {
            eprintln!(
                "[daemon] 后台周期任务已在配置中关闭（{}）",
                config::daemon_config_file().display()
            );
            eprintln!("[daemon] 要重新开启：把该文件里的 backgroundTasks 设为 true。");
            return;
        }
        Err(error) => {
            eprintln!("[daemon] {}", error.message());
            std::process::exit(1);
        }
    }

    println!("workbuddy-switch daemon v{}", update::APP_VERSION);
    println!("后台任务已启动（签到 / 旅行 / 轮换 / 保活 / 限额监听），按 Ctrl+C 停止。");
    println!("（要优雅停止请运行 `workbuddy-switch daemon --stop`）");

    // 循环都跑在后台任务里；主任务挂起直到进程被终止（Ctrl+C / SIGTERM）。
    std::future::pending::<()>().await;
}

/// `hook-record`：从 stdin 读 hook payload，记录「当前对话」指针。
///
/// **fail-open 是本函数的契约**：hook 挂在用户每一次对话结束（Stop）上，任何异常都
/// 只写 stderr 并以退出码 0 结束——绝不能因为我们的记录失败而影响客户端正常收尾。
/// stdout 固定输出 `{}`：客户端会把**空 stdout 当作 hook 失败**（见 core 里
/// `rate_limit_hook` 的同款结论），所以这里必须出声，且内容对协议是中性的。
fn hook_record() {
    use std::io::Read;

    let mut payload = String::new();
    if std::io::stdin().read_to_string(&mut payload).is_err() {
        println!("{{}}");
        return;
    }

    let trimmed = payload.trim();
    if !trimmed.is_empty() {
        match serde_json::from_str::<serde_json::Value>(trimmed) {
            Ok(value) => {
                if let Err(error) = active_session::record_hook_payload(&value) {
                    eprintln!("[hook-record] 记录当前对话失败: {error}");
                }
            }
            Err(error) => eprintln!("[hook-record] payload 不是合法 JSON: {error}"),
        }
    }

    println!("{{}}");
}

async fn serve(args: &[String]) {
    let mut port = default_port();
    if let Some(i) = args.iter().position(|a| a == "--port") {
        if let Some(p) = args.get(i + 1).and_then(|p| p.parse::<u16>().ok()) {
            port = p;
        }
    }

    // 访问令牌：父进程可用 WB_WEBUI_TOKEN 预置，否则现场生成。
    // **绑定端口前就写盘**——MCP 的 `wb_open_webui` 拉起服务后要靠这个文件拿令牌，
    // 才能打开一个能通过校验的链接。
    let token = std::env::var("WB_WEBUI_TOKEN")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(config::generate_webui_token);
    api::set_webui_token(token.clone());
    if let Err(error) = config::save_webui_token(port, &token) {
        eprintln!("[webui] 写入访问令牌失败：{error}（服务照常启动，但界面可能无法通过校验）");
    }

    let app = api::router();
    let addr = format!("127.0.0.1:{port}");
    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("启动失败: 端口 {port} 被占用或不可用（{e}）。可用 --port 指定其他端口。");
            std::process::exit(1);
        }
    };

    let url = format!("http://{addr}/?token={token}");
    println!("workbuddy-switch v{}", update::APP_VERSION);
    println!("webui: {url}");
    println!("按 Ctrl+C 停止服务。");

    let no_open = args.iter().any(|a| a == "--no-open");
    if !no_open {
        open_browser_url(&url);
    }

    spawn_background_loops();

    // 收尾失败不该 panic：端口被抢、连接中断都是可预期的运行期错误。
    if let Err(error) = axum::serve(listener, app).await {
        eprintln!("服务异常退出: {error}");
        std::process::exit(1);
    }
}

/// 用系统默认浏览器打开完整 URL（含访问令牌）。
///
/// `pub(crate)`：MCP 的 `wb_open_webui` 复用同一套跨平台打开逻辑。
pub(crate) fn open_browser_url(url: &str) {
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(url).spawn();
    }
    #[cfg(target_os = "windows")]
    {
        let mut c = std::process::Command::new("cmd");
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW：开浏览器不闪 cmd 窗
        }
        let _ = c.args(["/C", "start", url]).spawn();
    }
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("xdg-open").arg(url).spawn();
    }
}

#[cfg(test)]
mod tests {
    use super::variant_arg;
    use wb_switch_core::modules::variant::WbVariant;

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    /// 不传档位时必须仍是国内版（改造前行为）。
    #[test]
    fn cli_variant_defaults_to_cn() {
        assert_eq!(variant_arg(&args(&["status"])), WbVariant::Cn);
        assert_eq!(variant_arg(&args(&["status", "--debug"])), WbVariant::Cn);
        assert_eq!(variant_arg(&args(&["status", "--variant"])), WbVariant::Cn);
        assert_eq!(variant_arg(&args(&["status", "cn"])), WbVariant::Cn);
        assert_eq!(
            variant_arg(&args(&["status", "--variant=cn"])),
            WbVariant::Cn
        );
    }

    /// 已下线的国际版取值（空格 / 等号 / 大小写）一律回落国内版。
    #[test]
    fn cli_variant_ignores_retired_ai_forms() {
        assert_eq!(
            variant_arg(&args(&["status", "--variant", "ai"])),
            WbVariant::Cn
        );
        assert_eq!(
            variant_arg(&args(&["status", "--variant=ai"])),
            WbVariant::Cn
        );
        assert_eq!(
            variant_arg(&args(&["status", "--variant", "AI", "--no-open"])),
            WbVariant::Cn
        );
        assert_eq!(
            variant_arg(&args(&["--variant=ai", "status"])),
            WbVariant::Cn
        );
    }
}
