//! CLI 行为回归测试。
//!
//! 这些契约靠「跑一遍真的二进制」才能验证，单测里的函数调用覆盖不到参数分派。

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// 测试专用的隔离主目录。
///
/// **必须隔离，不是可选项**：`serve` 会启动后台周期任务——签到、派旅行、自动轮换，
/// 以及限额 hook 的「默认接入」（往 `~/.workbuddy/settings.json`、`~/.codebuddy/settings.json`
/// 里注册 Stop hook）。若用真实主目录跑，这些都会落到用户的真实环境里：改写他们的
/// 客户端设置、用他们的账号发请求。这不是「测试的副作用」，是事故。
///
/// Windows 上 `dirs::home_dir()` 走 `SHGetKnownFolderPath`，不认 `USERPROFILE`，
/// 所以唯一可行的隔离手段是 `WB_SWITCH_HOME`（见 core 的 `config::home_dir`）。
struct IsolatedHome(PathBuf);

impl IsolatedHome {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("wb-switch-cli-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("创建隔离主目录");
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for IsolatedHome {
    fn drop(&mut self) {
        // 尽力而为：Windows 上刚被 kill 的子进程可能仍持有句柄，失败不影响断言。
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// 未知子命令必须**报错退出**，绝不能兜底成 `serve`。
///
/// 这条曾经真的踩到：旧版内核被插件以 `hook-record` 拉起时，落进了
/// `_ => serve(&args).await`，于是静默启动一个常驻 web 服务器并一直阻塞。
/// 而 `hook-record` 挂在每轮对话结束（Stop）上，在真实客户端里会表现为
/// **每轮都卡到 hook 超时**——而且当时从输出上看不出任何异常。
#[test]
fn unknown_subcommand_fails_instead_of_starting_server() {
    let home = IsolatedHome::new("unknown-subcommand");
    let exe = env!("CARGO_BIN_EXE_wb-switch");

    let mut child = Command::new(exe)
        .arg("definitely-not-a-subcommand")
        .env("WB_SWITCH_HOME", home.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("应能启动 wb-switch 二进制");

    // 必须自己带超时：一旦回归成 serve，进程不会退出，直接 wait 会把测试挂死，
    // 在 CI 上表现为「超时」而不是「失败」，反而更难定位。
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match child.try_wait().expect("查询子进程状态失败") {
            Some(_) => break,
            None if Instant::now() > deadline => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("未知子命令没有退出——很可能又兜底成了 serve（常驻服务器）");
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    }

    let output = child.wait_with_output().expect("收集子进程输出失败");
    assert_eq!(
        output.status.code(),
        Some(2),
        "未知子命令应以退出码 2 结束，实际为 {:?}",
        output.status.code()
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("webui"),
        "不该启动 webui（serve 的标志输出），实际 stdout: {stdout}"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("未知子命令"),
        "stderr 应说明是未知子命令，实际: {stderr}"
    );
}

/// 无参数仍等同 `serve`（既有命令行习惯）：只验证它**不会**被判成未知子命令。
///
/// 用 `--port 0` 让内核绑定任意空闲端口，避免和本机已在跑的服务撞车；
/// 主目录必须隔离（见 [`IsolatedHome`]）。
#[test]
fn no_subcommand_still_defaults_to_serve() {
    let home = IsolatedHome::new("default-serve");
    let exe = env!("CARGO_BIN_EXE_wb-switch");

    let mut child = Command::new(exe)
        .args(["--port", "0", "--no-open"])
        .env("WB_SWITCH_HOME", home.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("应能启动 wb-switch 二进制");

    // 给它一点时间：如果误判成「未知子命令」会立刻退出 2。
    std::thread::sleep(Duration::from_secs(3));
    let status = child.try_wait().expect("查询子进程状态失败");

    let _ = child.kill();
    let output = child.wait_with_output().expect("收集子进程输出失败");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert_ne!(
        status.and_then(|s| s.code()),
        Some(2),
        "无参数不该被判成未知子命令。stdout: {stdout}"
    );
    assert!(
        stdout.contains("webui") || status.is_none(),
        "无参数应进入 serve（打印 webui 地址或仍在运行），实际 stdout: {stdout}"
    );
}
