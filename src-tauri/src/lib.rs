// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
mod commands;
mod companion;
#[cfg(target_os = "macos")]
mod instance_lock;
#[cfg(desktop)]
mod tray;
mod update_service;

use tauri::Emitter;
use wb_switch_core::modules;

const SCREENSHOT_DEMO_ENV: &str = "WB_SWITCH_SCREENSHOT_DEMO";

pub(crate) fn is_screenshot_demo() -> bool {
    std::env::var(SCREENSHOT_DEMO_ENV).as_deref() == Ok("1")
}

/// 轮换推迟提示：桌面端先向前端推 `rotate-deferred`（应用内提示，窗口开着就能看到），
/// 再尽力投递系统通知（应用在托盘/后台时可见）。
///
/// 应用内提示不依赖系统通知权限：插件在开发态会把通知登记到「终端」名下，且投递失败
/// 无法观测（`show()` 恒返回 Ok），所以两者都发、以前者为准。
/// 其它形态由 core 的日志与 `notify` 返回字段承载，宿主不投递。
pub(crate) fn deliver_rotate_notify(app: &tauri::AppHandle, result: &serde_json::Value) {
    #[cfg(desktop)]
    {
        if let Some(notify) = result.get("notify") {
            let _ = app.emit("rotate-deferred", notify.clone());
            tray::notify_rotate_deferred(app, notify);
        }
    }
    #[cfg(not(desktop))]
    {
        let _ = (app, result);
    }
}

/// 后台循环：签到 / 旅行 / 轮换 / 保活 / 限额 hook 监听。
///
/// 周期任务本体在 core 的 `daemon` 模块，与 `wb-switch daemon`、`wb-switch serve`
/// 共用同一份实现，并由**跨进程锁**选出唯一执行者——桌面版与插件拉起的守护进程
/// 同时开着时，不会重复签到、重复轮换、重复派旅行。
///
/// 拿不到锁时桌面版只提供界面能力。**已知取舍**：轮换推迟提示原本由本进程经 Tauri
/// 事件推给前端，这时不会出现（轮换日志与 `wb_rotate_status` 仍可查）。桌面版下线后
/// 这个差异自然消失。
fn spawn_background_loops(app: tauri::AppHandle) {
    let event_app = app.clone();
    let notify_app = app.clone();
    match modules::daemon::start_if_elected(
        move || {
            let _ = event_app.emit("rate-limits-updated", serde_json::json!({}));
        },
        move |notify| {
            // 复用既有的投递路径（应用内提示 + 尽力而为的系统通知），避免在这里
            // 重复一遍 cfg 分支；该函数只认 `result.notify`，故包一层。
            deliver_rotate_notify(&notify_app, &serde_json::json!({ "notify": notify }));
        },
    ) {
        Ok(()) => {}
        Err(modules::daemon::DaemonLockError::Busy) => {
            eprintln!("[后台任务] 已有进程在运行，桌面版只提供界面，不重复跑周期任务");
        }
        Err(error) => {
            eprintln!("[后台任务] {}，本进程不跑周期任务", error.message());
        }
    }

    // 统一更新服务：桌面宿主自身的能力（托盘菜单要反映阶段），刻意**不**纳入上面的
    // 「唯一执行者」——它不写账号侧数据，重复检查只是多一次网络请求。
    // 首次 15 秒后检查一次，之后每 30 分钟（未带 force，走 core 的 6 小时缓存）。
    update_service::spawn_periodic_check(app);
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let mut builder = tauri::Builder::default();

    // 单实例互斥必须最先注册：`Builder::build()` 按注册顺序 initialize_plugins，
    // 插件 setup 命中已有实例会直接 `std::process::exit(0)`，因此第二个进程在
    // 建主窗口 / 建托盘图标 / 起后台循环之前就已退出，不会产生账号侧副作用。
    #[cfg(desktop)]
    {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            tray::on_second_instance(app, args);
        }));
    }

    builder = builder
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init());

    #[cfg(desktop)]
    if !is_screenshot_demo() {
        builder = builder.plugin(agent_studio_desktop::init(companion::config()));
    }

    #[cfg(desktop)]
    {
        builder = builder.plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec![tray::SILENT_STARTUP_ARG]),
        ));
        builder = builder.on_window_event(tray::on_window_event);
    }

    let app = builder
        .setup(|app| {
            #[cfg(desktop)]
            {
                // 插件已在 initialize_plugins 阶段决定 notify-or-exit；此处只兜底
                // 插件漏掉的 macOS 竞态。必须在 tray::setup 之前：拿不到锁的第二
                // 实例不能先建出托盘图标。不得放到 run() 开头，否则会抢在插件
                // notify 之前拦下正常第二实例，丢掉「再点开 → 既有窗口弹出」。
                #[cfg(target_os = "macos")]
                instance_lock::acquire_or_exit(app.handle());
                tray::setup(app)?;
                // 主窗口由配置创建为不可见；在事件循环呈现前决定本次启动是否静默。
                // 仅系统自启（精确 `--hidden` 参数）进入静默托盘，普通启动立即显示主窗口。
                tray::setup_startup_visibility(
                    app.handle(),
                    tray::is_silent_startup(std::env::args()),
                );
            }
            // README 截图模式只渲染前端虚构数据，禁止读取账号后执行签到、轮换或保活。
            if !is_screenshot_demo() {
                spawn_background_loops(app.handle().clone());
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_status,
            commands::get_accounts,
            commands::get_codebuddy_cli_status,
            commands::install_codebuddy_cli_helper,
            commands::switch_codebuddy_cli_account,
            commands::get_codebuddy_cn_ide_status,
            commands::switch_codebuddy_cn_ide_account,
            commands::detect_codebuddy_cn_ide_account,
            commands::list_codebuddy_ide_sessions,
            commands::codebuddy_ide_session_links_preview,
            commands::get_vscode_ext_status,
            commands::switch_vscode_ext_account,
            commands::detect_vscode_ext_account,
            commands::list_vscode_sessions,
            commands::vscode_session_links_preview,
            commands::get_codebuddy_ide_status,
            commands::switch_codebuddy_ide_account,
            commands::list_codebuddy_intl_ide_sessions,
            commands::codebuddy_intl_ide_session_links_preview,
            commands::detect_codebuddy_ide_account,
            commands::get_jetbrains_status,
            commands::switch_jetbrains_account,
            commands::detect_jetbrains_account,
            commands::delete_account,
            commands::oauth_start,
            commands::oauth_status,
            commands::import_local,
            commands::export_accounts,
            commands::export_accounts_to_path,
            commands::preview_import_accounts,
            commands::import_accounts,
            commands::switch_account,
            commands::list_sessions,
            commands::copy_sessions,
            commands::session_links_preview,
            commands::open_permission_settings,
            commands::check_auth_permission,
            commands::reveal_app_in_finder,
            commands::get_checkin_status,
            commands::get_credit_expiry,
            commands::get_credit_statistics,
            commands::get_token_statistics,
            commands::get_rate_limits,
            commands::get_rate_limit_hook_status,
            commands::install_rate_limit_hook,
            commands::uninstall_rate_limit_hook,
            commands::get_rate_limit_config,
            commands::save_rate_limit_config,
            commands::checkin,
            commands::checkin_all,
            commands::get_auto_checkin_config,
            commands::save_auto_checkin_config,
            commands::get_checkin_logs,
            commands::get_travel_status,
            commands::get_auto_travel_config,
            commands::save_auto_travel_config,
            commands::refresh_account_token,
            commands::get_auto_rotate_config,
            commands::save_auto_rotate_config,
            commands::rotate_status,
            commands::run_rotate,
            commands::get_rotate_logs,
            commands::get_github_config,
            commands::save_github_config,
            commands::check_update,
            commands::update_state,
            commands::update_download,
            commands::update_restart,
            commands::relaunch_app,
            commands::get_launch_at_login_enabled,
            commands::set_launch_at_login_enabled,
            commands::record_notification,
            commands::list_notifications,
            commands::clear_notifications,
            commands::log_error,
            commands::get_error_log_path,
            commands::reveal_error_log,
            companion::get_companion_enabled,
            companion::set_companion_enabled,
            companion::open_companion_settings,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    app.run(|_app_handle, event| {
        #[cfg(desktop)]
        {
            // 点击 Dock / Finder 再次激活已运行实例：窗口已隐藏到托盘时显示主窗口。
            // `Reopen` 在主线程派发，可直接调用窗口路径。
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Reopen {
                has_visible_windows: false,
                ..
            } = &event
            {
                tray::show_main_window_on_reopen(_app_handle);
            }
            tray::on_run_event(event);
        }
    });
}
