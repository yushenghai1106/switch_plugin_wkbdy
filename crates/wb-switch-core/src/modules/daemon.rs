//! 后台周期任务（签到 / 派猫猫旅行 / 自动轮换 / 保活 / 限额 hook 监听）。
//!
//! 谁跑这些任务：**同一时刻只能有一个宿主跑**。桌面版、`wb-switch serve`、插件拉起的
//! `wb-switch daemon` 三者的循环内容完全一致，若同时跑就会重复签到、重复轮换、
//! 重复派旅行。因此用一把跨进程锁选出一个执行者，其余宿主退化为「只提供各自的
//! 交互能力，不跑循环」。
//!
//! 锁的落点是 `<store>/daemon.lock`，**刻意不复用 `instance.lock`**：后者是桌面版的
//! 单实例锁，语义是「已有实例就退出进程」（见 `src-tauri/src/instance_lock.rs`）。
//! 若把循环执行权也挂在它上面，插件拉起的 daemon 一旦持锁，用户再打开桌面版就会被
//! 误判成第二实例而直接退出——那是个真实的回归，不是理论风险。
//!
//! 锁原语直接复用 `session_link::try_lock_file`（`std::fs::File::try_lock`，
//! Rust 1.89 起稳定，Windows/Unix 均由标准库实现），进程退出或崩溃时由内核释放，
//! 不会留下「锁文件还在但没人持锁」的假占用。

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use serde_json::Value;

use crate::modules::session_link::{try_lock_file, FileLock, LockError};
use crate::modules::{
    checkin, config, limits, rate_limit_events, rate_limit_hook, refresh, rotate, travel,
};

/// 循环执行权锁文件名（`store_dir()/daemon.lock`）。
pub const DAEMON_LOCK_FILE_NAME: &str = "daemon.lock";

/// 轮换与保活的统一节拍：30 秒醒来一次，各自按配置间隔决定是否真要跑。
const TICK: Duration = Duration::from_secs(30);

/// 持有后台循环执行权的凭据；**Drop 即释放**，因此必须活到进程结束。
pub struct DaemonGuard {
    /// 字段本身不被读取，存在的意义就是保住 fd（Drop 时解锁）。
    _lock: FileLock,
    /// 记录持锁进程 PID 的文件（诊断用：能一眼看出是谁在跑循环）。
    /// 锁文件本身只有互斥语义，不含内容，所以 PID 另写一份。
    pid_file: Option<PathBuf>,
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        if let Some(path) = &self.pid_file {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// 抢锁失败的原因。
#[derive(Debug)]
pub enum DaemonLockError {
    /// 已有别的进程在跑循环（正常情况：谁先起谁跑）。
    Busy,
    /// 无法建立互斥（目录不可写等）。此时**不跑**循环——否则插件每次会话启动都会
    /// 拉起一个新 daemon，演变成多个循环并发跑，正是这把锁要防的事。
    Unavailable(String),
}

impl DaemonLockError {
    /// 面向日志/报错的一句话说明。
    pub fn message(&self) -> String {
        match self {
            Self::Busy => "已有进程在运行后台任务".to_string(),
            Self::Unavailable(reason) => format!("无法建立后台任务互斥：{reason}"),
        }
    }
}

/// 锁文件路径。
pub fn daemon_lock_path() -> PathBuf {
    config::store_dir().join(DAEMON_LOCK_FILE_NAME)
}

/// PID 记录文件路径（与锁文件同目录同名不同后缀）。
fn pid_file_path() -> PathBuf {
    daemon_lock_path().with_extension("pid")
}

/// 尝试取得后台循环执行权（生产入口）。
pub fn try_acquire() -> Result<DaemonGuard, DaemonLockError> {
    try_acquire_at(&daemon_lock_path())
}

/// [`try_acquire`] 的可测实现：显式传入锁文件路径。
pub fn try_acquire_at(path: &Path) -> Result<DaemonGuard, DaemonLockError> {
    match try_lock_file(path) {
        Ok(lock) => {
            // 顺手写下 PID：写失败不影响互斥（锁已经拿到），因此不当作错误。
            let pid_file = path.with_extension("pid");
            let pid_file = std::fs::write(&pid_file, format!("{}\n", std::process::id()))
                .is_ok()
                .then_some(pid_file);
            Ok(DaemonGuard {
                _lock: lock,
                pid_file,
            })
        }
        Err(LockError::Busy) => Err(DaemonLockError::Busy),
        Err(LockError::Unavailable(reason)) => Err(DaemonLockError::Unavailable(reason)),
    }
}

// ---------------------------------------------------------------------------
// 查询与停止
// ---------------------------------------------------------------------------

/// 正在跑循环的进程 PID（没有则 `None`）。
///
/// 判据是**锁**而不是 PID 文件：PID 文件只是诊断信息，进程被强杀时不会清理，
/// 单独拿它当判据会把「已死进程」当成在运行。
pub fn running_pid() -> Option<u32> {
    match try_acquire() {
        // 拿得到锁 → 没人在跑（guard 在本函数返回时释放）。
        Ok(_) => None,
        Err(DaemonLockError::Busy) => read_pid().ok(),
        Err(DaemonLockError::Unavailable(_)) => None,
    }
}

fn read_pid() -> Result<u32, String> {
    let raw = std::fs::read_to_string(pid_file_path())
        .map_err(|error| format!("读取 PID 文件失败：{error}"))?;
    raw.trim()
        .parse::<u32>()
        .map_err(|_| format!("PID 文件内容无法解析：{raw:?}"))
}

/// 请求结束正在运行的守护进程。
///
/// 安全前提：**只在确认锁被持有之后才动 PID**。PID 会被系统复用，PID 文件也可能因
/// 强杀而残留，所以「拿得到锁 = 没人跑 = 不杀任何东西」这一步不能省——否则可能误杀
/// 一个恰好复用了同一 PID 的无关进程。
///
/// 建立不了互斥时（Unavailable）同样不杀：无法确认状态就不该动手。
pub fn stop_running() -> Result<String, String> {
    match try_acquire() {
        Ok(_) => {
            // 没人在跑：顺手清掉可能残留的 PID 文件。
            let _ = std::fs::remove_file(pid_file_path());
            Ok("后台任务当前没有在运行".to_string())
        }
        Err(DaemonLockError::Busy) => {
            let pid = read_pid()?;
            terminate(pid)?;
            // 等锁真正释放再返回，否则调用方紧接着重启仍会撞上 Busy。
            if wait_until_lock_free(Duration::from_secs(10)) {
                Ok(format!("已结束后台任务进程（PID {pid}）"))
            } else {
                Err(format!("已请求结束 PID {pid}，但它仍在运行"))
            }
        }
        Err(DaemonLockError::Unavailable(reason)) => Err(format!(
            "无法确认后台任务是否在运行（{reason}），出于安全没有结束任何进程"
        )),
    }
}

/// 轮询直到锁可获取（= 之前的持有者已退出）。
fn wait_until_lock_free(timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if matches!(try_acquire(), Ok(_) | Err(DaemonLockError::Unavailable(_))) {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// 结束指定进程。`/T`（Windows）连带子进程，避免守护拉起的子进程残留。
#[cfg(target_os = "windows")]
fn terminate(pid: u32) -> Result<(), String> {
    let output = std::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .output()
        .map_err(|error| format!("调用 taskkill 失败：{error}"))?;
    if output.status.success() {
        return Ok(());
    }
    Err(format!(
        "结束进程 {pid} 失败：{}",
        String::from_utf8_lossy(&output.stderr).trim()
    ))
}

#[cfg(not(target_os = "windows"))]
fn terminate(pid: u32) -> Result<(), String> {
    let output = std::process::Command::new("kill")
        .arg(pid.to_string())
        .output()
        .map_err(|error| format!("调用 kill 失败：{error}"))?;
    if output.status.success() {
        return Ok(());
    }
    Err(format!(
        "结束进程 {pid} 失败：{}",
        String::from_utf8_lossy(&output.stderr).trim()
    ))
}

/// 启动全部周期任务。
///
/// `guard` **按值传入并按值返回**：这样「跑循环」与「持有执行权」在类型上绑死，
/// 调用方不可能忘记保活 guard（忘了就编译不过），也不可能在没有锁的情况下起循环。
///
/// - `on_rate_limit_event`：限额 hook 有新入账时回调（桌面版借它推事件给前端；
///   无 UI 的宿主传空实现即可）。
/// - `on_rotate_notify`：轮换被推迟时的提示回调。桌面版把它转成应用内 toast；
///   无 UI 的宿主不需要——轮换日志里已经记了同样的内容。
pub fn spawn_loops(
    guard: DaemonGuard,
    on_rate_limit_event: impl Fn() + Send + Sync + 'static,
    on_rotate_notify: impl Fn(&Value) + Send + Sync + 'static,
) -> DaemonGuard {
    let notify = Arc::new(on_rotate_notify);

    // 签到：启动即核验一轮，之后按 core 算出的下一轮延迟睡眠（未设置时间段时 30 分钟）。
    // 先整理历史日志，避免长期运行后日志文件无限增长。
    tokio::spawn(async move {
        if let Err(error) = config::compact_checkin_logs() {
            eprintln!("[签到] 历史日志整理失败: {error}");
        }
        let _ = checkin::run_checkin_cycle(checkin::CheckinCycleMode::StartupVerify).await;
        loop {
            tokio::time::sleep(checkin::next_cycle_delay()).await;
            let _ = checkin::run_checkin_cycle(checkin::CheckinCycleMode::PeriodicRecovery).await;
        }
    });

    // 派猫猫旅行：启动即派发，之后周期性补派（并重试 no-buddy / 瞬时错误）。
    // 档位过滤在 core（`travel_capable_accounts`）：不支持成长中心的档位不会发请求。
    tokio::spawn(async move {
        let _ = travel::run_travel_cycle().await;
        loop {
            tokio::time::sleep(travel::TRAVEL_RETRY_INTERVAL).await;
            let _ = travel::run_travel_cycle().await;
        }
    });

    // 旅行领取：启动立刻查一轮（避免重启后空等一个周期漏领），之后按周期检查。
    tokio::spawn(async move {
        let _ = travel::run_travel_claim_cycle().await;
        loop {
            tokio::time::sleep(travel::TRAVEL_CLAIM_INTERVAL).await;
            let _ = travel::run_travel_claim_cycle().await;
        }
    });

    // 轮换 + 保活：同一个 30 秒节拍里各自判断是否到期。
    let rotate_notify = notify.clone();
    tokio::spawn(async move {
        let mut last_keepalive_day = String::new();
        let mut last_rotate_at: i64 = 0;
        loop {
            let rotate_cfg = config::load_auto_rotate_config();
            if rotate_cfg.get("enabled").and_then(|v| v.as_bool()) == Some(true) {
                let interval_minutes = rotate_cfg
                    .get("check_interval_minutes")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(5)
                    .max(1);
                let now = config::now_ms();
                if now - last_rotate_at >= interval_minutes * 60_000 {
                    last_rotate_at = now;
                    let result = rotate::run_rotate_cycle().await;
                    if let Some(message) = result.get("notify") {
                        rotate_notify(message);
                    }
                }
            }

            let today = checkin::date_str(None);
            if today != last_keepalive_day {
                last_keepalive_day = today;
                let _ = refresh::run_keepalive_cycle().await;
            }

            tokio::time::sleep(TICK).await;
        }
    });

    // 限额 hook 信号：轮询 `~/.wb-switch/hook-events.jsonl`（客户端 hook 追加），
    // 入账后通知宿主立刻拉取。
    rate_limit_events::spawn_watcher(on_rate_limit_event);

    // 默认接入：后台线程自动安装 hook（幂等、非阻塞、失败静默）。
    // 前置条件（开关开启 / 用户没卸载过 / 存在客户端 / 未装全）由 core 判定；
    // 装上了就作废扫描缓存——扫描范围从全量收窄到「未注册的来源」。
    std::thread::spawn(|| {
        if rate_limit_hook::auto_install_on_startup() {
            limits::invalidate_scan_cache();
        }
    });

    guard
}

/// 宿主便捷入口：抢到执行权就在本进程启动全部周期任务。
///
/// guard 存进**进程级静态变量**，而不是留在宿主函数的局部变量里：循环启动完宿主就
/// 返回了，局部变量会立刻 drop 掉锁，于是「本进程还在跑循环但锁已经放了」——下一个
/// 宿主会以为没人跑而再起一套。这个坑靠调用方自觉不可靠，所以由本函数负责持有。
///
/// 同一进程重复调用是幂等的（第二次直接返回 Ok）。看到 [`DaemonLockError::Busy`]
/// 说明**别的进程**在跑循环，属于正常情况，宿主应当安静退化为「不跑循环」。
pub fn start_if_elected(
    on_rate_limit_event: impl Fn() + Send + Sync + 'static,
    on_rotate_notify: impl Fn(&Value) + Send + Sync + 'static,
) -> Result<(), DaemonLockError> {
    static HELD: OnceLock<DaemonGuard> = OnceLock::new();

    if HELD.get().is_some() {
        return Ok(());
    }
    let guard = try_acquire()?;
    let guard = spawn_loops(guard, on_rate_limit_event, on_rotate_notify);
    let _ = HELD.set(guard);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_lock_path(name: &str) -> PathBuf {
        std::env::temp_dir()
            .join(format!(
                "wb-switch-daemon-lock-{}-{name}",
                std::process::id()
            ))
            .join(DAEMON_LOCK_FILE_NAME)
    }

    fn cleanup(path: &Path) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::remove_dir_all(parent);
        }
    }

    /// 持锁期间第二次获取必须失败——这正是「防双跑」的核心契约。
    #[test]
    fn second_acquire_is_busy_while_first_holds() {
        let path = temp_lock_path("busy");
        cleanup(&path);

        let first = try_acquire_at(&path).expect("首个获取应成功");
        match try_acquire_at(&path) {
            Err(DaemonLockError::Busy) => {}
            Ok(_) => panic!("持锁期间第二个获取不该成功"),
            Err(other) => panic!("应报 Busy，实际 {other:?}"),
        }

        drop(first);
        cleanup(&path);
    }

    /// Drop 之后必须能重新获取（进程退出/崩溃由内核释放锁，不能留下假占用）。
    #[test]
    fn lock_is_released_on_drop() {
        let path = temp_lock_path("release");
        cleanup(&path);

        let first = try_acquire_at(&path).expect("首个获取应成功");
        drop(first);

        let _second = try_acquire_at(&path).expect("释放后应能重新获取");
        cleanup(&path);
    }

    /// 父目录不存在时应自动创建（首次运行时 `~/.wb-switch` 可能还没有）。
    #[test]
    fn parent_directory_is_created() {
        let path = temp_lock_path("create-dir");
        cleanup(&path);
        assert!(!path.parent().unwrap().exists());

        let _guard = try_acquire_at(&path).expect("应自动创建父目录并获取");
        assert!(path.parent().unwrap().is_dir());
        cleanup(&path);
    }

    /// 父路径被占成普通文件时属于「无法建立互斥」：必须**不跑**循环，而不是当作空闲。
    #[test]
    fn unusable_lock_path_is_unavailable() {
        let path = temp_lock_path("unavailable");
        cleanup(&path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let blocker = path.parent().unwrap().join("blocker");
        std::fs::write(&blocker, b"not a dir").unwrap();
        // 把锁文件路径指到一个「父是普通文件」的位置。
        let bad = blocker.join(DAEMON_LOCK_FILE_NAME);

        match try_acquire_at(&bad) {
            Err(DaemonLockError::Unavailable(_)) => {}
            Ok(_) => panic!("父路径不是目录时不该拿到锁"),
            Err(other) => panic!("应报 Unavailable，实际 {other:?}"),
        }
        cleanup(&path);
    }

    /// 报错文案必须说清是「已有进程在跑」还是「建立不了互斥」——两者处置完全不同。
    #[test]
    fn error_messages_distinguish_the_two_causes() {
        assert!(DaemonLockError::Busy.message().contains("已有进程"));
        assert!(DaemonLockError::Unavailable("权限不足".to_string())
            .message()
            .contains("权限不足"));
    }
}
