//! 「当前对话」指针：插件 hook 落盘、导出工具读取。
//!
//! 为什么需要它：WorkBuddy / CodeBuddy 的 hook payload 里带 `transcript_path` 与
//! `session_id`，但 hook 是**一次性进程**，跑完就没了；而用户点「导出当前对话」是在
//! 之后的另一次工具调用里。所以 hook 必须把「现在是哪个对话」记下来，导出时再读。
//!
//! 与 `rate_limit_events` 的区别：那边关心的是「限额事件」，这里关心的是「对话位置」，
//! 两者的写入频率、生命周期和消费方都不同，因此各用各的文件，互不干扰。
//!
//! 档位判定不看 payload 自报（payload 里没有档位字段），而是看 `transcript_path`
//! **实际落在哪个档位的 `projects/` 目录下**——这是唯一可靠的判据，也顺带挡掉了
//! 非 WorkBuddy 会话（例如 CodeBuddy CLI 的转录文件）被误当成可导出会话。

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::modules::config::{self, atomic_write};
use crate::modules::session::SessionPaths;
use crate::modules::variant::WbVariant;
use crate::modules::{account, process, session, switch, vscode_ext, vscode_session};

/// 指针文件名（落在工具存储根下）。
const FILE_NAME: &str = "active-session.json";

/// 一次「当前对话」的记录。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CurrentConversation {
    /// 会话 id：等于会话正文 `{cid}.jsonl` 的文件名主干，也是 `workbuddy.db` 的主键。
    pub session_id: String,
    /// 会话正文的绝对路径。
    pub transcript: PathBuf,
    /// 会话所属工作目录（payload 里的 `cwd`，可能为空）。
    pub cwd: String,
    /// 来自哪个档位。
    pub variant: WbVariant,
}

/// 记录 hook payload。返回一段说明（写成功 / 跳过原因），供 hook 脚本决定要不要出声。
///
/// 非 WorkBuddy 会话（例如 CodeBuddy CLI 的转录）会**静默跳过**而不是报错：hook 在
/// 各客户端上都会触发，报错只会刷屏且没有可操作性。
pub fn record_hook_payload(payload: &Value) -> Result<Value, String> {
    record_at(&config::store_dir(), payload, &real_roots())
}

/// 读取「当前对话」；没有记录或记录已失效（文件被删）时返回 `None`。
///
/// 注意：本函数读的是**真实**存储根。需要按档位过滤或注入路径时，用
/// [`current_at`] 自己过滤——`export_to_account_at` 就是这么做的。
pub fn current() -> Option<CurrentConversation> {
    current_at(&config::store_dir())
}

fn pointer_path(store_root: &Path) -> PathBuf {
    store_root.join(FILE_NAME)
}

/// 把「当前对话」导出给目标账号。
///
/// `switch = false`：只复制会话。这条路径**要求 WorkBuddy 未运行**——复制会写
/// `projects/*.jsonl` 与 `workbuddy.db`，而运行中的 App 把这些缓存在内存里，
/// 稍后会用自己的状态覆盖回去。`session::copy_sessions_for_switch` 自带这道闸，
/// 这里只在被拦下时补一句可操作的建议。
///
/// `switch = true`：切换账号并把当前对话一起带过去。这条路径**不要求先退出 App**，
/// 因为切换流程本身就是「先关进程 → 再写会话与认证 → 最后重开」，写入发生在
/// App 停止写入之后。用户就坐在 WorkBuddy 里点「导给另一个号」时，走的是这条。
///
/// 两种模式都保证源账号数据不变、副本以新 id 落进目标账号（复用既有复制实现）。
pub fn export_to_account(target_acc: &Value, switch_account: bool) -> Result<Value, String> {
    let variant = account::variant_of(target_acc);
    let paths = SessionPaths::for_variant(variant);
    export_to_account_at(
        &paths,
        &config::store_dir(),
        target_acc,
        switch_account,
        process::is_workbuddy_running,
    )
}

/// [`export_to_account`] 的可测实现：路径、指针存储根与「App 是否运行」探针都可注入。
///
/// 与 `session::copy_sessions_for_switch` / `_at` 同一约定——生产入口只负责把真实的
/// 路径与进程探针接上，逻辑本体留在可注入的这一层，才能在临时目录里端到端验证。
pub(crate) fn export_to_account_at(
    paths: &SessionPaths,
    store_root: &Path,
    target_acc: &Value,
    switch_account: bool,
    is_app_running: impl Fn(WbVariant) -> bool,
) -> Result<Value, String> {
    let variant = account::variant_of(target_acc);
    // 档位必须一致：两个档位的数据根不同构，跨档位复制目标账号根本读不到。
    // 指针按 transcript 实际所在的 projects/ 判档，因此这里能准确拦住跨档位请求。
    let (current, resolved_by) = match current_at(store_root).filter(|c| c.variant == variant) {
        Some(current) => (current, "hook"),
        // 回退：hook 没生效（客户端没重启 / 未注册）时用「最近更新的带正文会话」。
        // 没有这层兜底的话，headline 功能在 hook 静默失效时会表现成「完全不可用」，
        // 而用户没有任何线索。会话列表本就按 updated_at 倒序，取第一条即可。
        None => (most_recent_with_history_at(paths, variant)?, "latest"),
    };

    let session_ids = vec![current.session_id.clone()];

    if switch_account {
        let account_id = target_acc
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if account_id.is_empty() {
            return Err("目标账号缺少 id，无法切换".to_string());
        }
        // 切换分支涉及关进程 / 写认证 / 重开，无法在注入路径上跑，因此固定走生产实现。
        // restart 必须为真：会话写入要在 App 停止之后进行，且写完要能回到可用状态。
        let mut result = switch::switch_account(None, &account_id, true, false, &session_ids, &[])?;
        annotate_export(&mut result, &current, resolved_by, true);
        return Ok(result);
    }

    match session::copy_sessions_for_switch_at(
        paths,
        variant,
        target_acc,
        &session_ids,
        is_app_running,
    ) {
        Ok(mut report) => {
            annotate_export(&mut report, &current, resolved_by, false);
            Ok(report)
        }
        Err(error) if error.contains(session::SESSION_COPY_APP_RUNNING) => Err(format!(
            "{error}；也可以改用「导出并切换」——切换流程会先关闭 WorkBuddy 再写入，无需手动退出"
        )),
        Err(error) => Err(error),
    }
}

/// 当前账号里最近更新、且确实带正文的会话（回退用）。
fn most_recent_with_history_at(
    paths: &SessionPaths,
    variant: WbVariant,
) -> Result<CurrentConversation, String> {
    let uid = session::current_user_uid_at(&paths.auth_file).ok_or_else(|| {
        format!(
            "未读取到 {} 档位的登录态，无法确定当前账号",
            variant.as_str()
        )
    })?;
    let sessions = session::list_sessions_for_user_at(paths, &uid);
    let item = sessions
        .as_array()
        .and_then(|list| {
            list.iter()
                .find(|s| s.get("hasHistory").and_then(Value::as_bool) == Some(true))
        })
        .ok_or_else(|| {
            "没有找到带正文的会话。请先在客户端里完成一轮对话，或确认当前账号有会话记录。"
                .to_string()
        })?;

    let session_id = item
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if session_id.is_empty() {
        return Err("会话记录缺少 id".to_string());
    }
    Ok(CurrentConversation {
        session_id,
        // 回退路径拿不到 transcript_path，留空；标注时会被跳过。
        transcript: PathBuf::new(),
        cwd: item
            .get("cwd")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        variant,
    })
}

/// 把 VS Code CodeBuddy 插件里**最近的对话**导给另一个账号。
///
/// 与 WorkBuddy 那条链路的区别在「如何确定当前对话」：VS Code 插件不触发 hook，
/// 我们拿不到 `transcript_path`，因此只能取扩展会话列表里**最近更新且带正文**的那条
/// ——这也正是用户视角的「我正在用的那个对话」。复制本身完全复用 `vscode_session`，
/// 本函数只负责选会话与拼 `CopyItem`，不重复实现写入逻辑。
///
/// 与 `copy_vscode_sessions` 相同的前置要求：**VS Code 必须完全退出**（否则运行中的
/// 扩展会用内存状态覆盖我们的写入），错误由下层原样透出。
pub fn export_vscode_conversation(target_acc: &Value) -> Result<Value, String> {
    let target_uid = target_acc
        .get("uid")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "目标账号缺少 uid，无法复制会话".to_string())?
        .to_string();

    let source_uid = vscode_ext::active_ext_uid().ok_or_else(|| {
        "未检测到 VS Code CodeBuddy 插件当前登录账号。请先在 VS Code 中登录该插件后重试。"
            .to_string()
    })?;

    let listed = vscode_session::list_vscode_sessions(&source_uid);
    let Some(session) = latest_conversation_with_history(&listed) else {
        return Err(
            "VS Code CodeBuddy 插件里没有找到带正文的对话，无法导出。请先在插件中开始一个对话。"
                .to_string(),
        );
    };

    let workspace_hash = session
        .get("workspaceHash")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let conversation_id = session
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if workspace_hash.is_empty() || conversation_id.is_empty() {
        return Err("会话记录缺少 workspaceHash 或 id，无法复制".to_string());
    }

    let items = vec![vscode_session::CopyItem {
        workspace_hash,
        conversation_id,
    }];
    let mut report = vscode_session::copy_vscode_sessions(&target_uid, &items)?;

    if report.is_object() {
        report["exportedConversationId"] = json!(session.get("id").cloned().unwrap_or(Value::Null));
        report["conversationTitle"] = json!(session
            .get("title")
            .cloned()
            .unwrap_or(Value::String("(无标题)".to_string())));
        report["sourceUid"] = json!(source_uid);
        report["targetUid"] = json!(target_uid);
        // 与 WorkBuddy 那条链路保持同一套字段名，便于上层统一展示。
        report["resolvedBy"] = json!("latest");
        report["switched"] = json!(false);
    }
    Ok(report)
}

/// 从 `list_vscode_sessions` 的结果里挑最近更新且带正文的会话。
///
/// 自己排序而不是依赖列表顺序：那个顺序由扩展数据目录的遍历顺序决定，不是契约。
fn latest_conversation_with_history(listed: &Value) -> Option<&Value> {
    let sessions = listed.get("sessions").and_then(Value::as_array)?;
    let mut candidates: Vec<&Value> = sessions
        .iter()
        .filter(|s| s.get("hasHistory").and_then(Value::as_bool) == Some(true))
        .collect();
    candidates.sort_by_key(|s| {
        std::cmp::Reverse(s.get("updatedAt").and_then(Value::as_i64).unwrap_or(0))
    });
    candidates.first().copied()
}

/// 在结果里补上「导出的是哪个会话」，便于调用方展示与排查。
fn annotate_export(
    result: &mut Value,
    current: &CurrentConversation,
    resolved_by: &str,
    switched: bool,
) {
    if !result.is_object() {
        return;
    }
    result["exportedSessionId"] = json!(current.session_id);
    if !current.transcript.as_os_str().is_empty() {
        result["transcript"] = json!(current.transcript.to_string_lossy());
    }
    result["sourceVariant"] = json!(current.variant.as_str());
    // `hook` = 由插件 hook 精确记录；`latest` = 回退取最近会话，便于用户判断是否符合预期。
    result["resolvedBy"] = json!(resolved_by);
    result["switched"] = json!(switched);
}

/// 真实档位数据根列表（生产用）。
fn real_roots() -> [(WbVariant, PathBuf); 2] {
    [
        (WbVariant::Cn, WbVariant::Cn.data_root()),
        (WbVariant::Ai, WbVariant::Ai.data_root()),
    ]
}

/// 可测实现：显式传入存储根与档位数据根，绝不触碰真实 `~/.wb-switch`。
fn record_at(
    store_root: &Path,
    payload: &Value,
    roots: &[(WbVariant, PathBuf)],
) -> Result<Value, String> {
    let transcript = payload
        .get("transcript_path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "payload 里没有 transcript_path".to_string())?;

    let Some(variant) = variant_of_transcript(transcript, roots) else {
        // 不属于任何 WorkBuddy 档位：不是可导出的会话，静默跳过。
        return Ok(json!({
            "recorded": false,
            "reason": "transcript 不在任何 WorkBuddy 档位的 projects/ 下",
            "transcriptPath": transcript,
        }));
    };

    // session_id 优先取 payload；缺失时用文件名主干兜底（正文文件名即会话 id）。
    let session_id = payload
        .get("session_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| {
            Path::new(transcript)
                .file_stem()
                .and_then(|s| s.to_str())
                .map(str::to_string)
        })
        .ok_or_else(|| "无法确定 session_id（payload 未提供且文件名无主干）".to_string())?;

    let cwd = payload
        .get("cwd")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    let record = json!({
        "sessionId": session_id,
        "transcriptPath": transcript,
        "cwd": cwd,
        "variant": variant.as_str(),
        "updatedAt": config::now_ms(),
    });

    std::fs::create_dir_all(store_root)
        .map_err(|e| format!("创建存储目录失败（{}）：{e}", store_root.display()))?;
    let body = serde_json::to_string_pretty(&record).map_err(|e| e.to_string())?;
    atomic_write(&pointer_path(store_root), &body)
        .map_err(|e| format!("写入当前对话指针失败：{e}"))?;

    Ok(json!({ "recorded": true, "pointer": record }))
}

fn current_at(store_root: &Path) -> Option<CurrentConversation> {
    let raw = std::fs::read_to_string(pointer_path(store_root)).ok()?;
    let value: Value = serde_json::from_str(&raw).ok()?;

    let session_id = value.get("sessionId").and_then(Value::as_str)?.to_string();
    let transcript = PathBuf::from(value.get("transcriptPath").and_then(Value::as_str)?);
    // 指针指向的文件可能已被删除或清理：此时视为没有当前对话，而不是给出一条死路。
    if !transcript.is_file() {
        return None;
    }
    let variant = WbVariant::parse(value.get("variant").and_then(Value::as_str));
    let cwd = value
        .get("cwd")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    Some(CurrentConversation {
        session_id,
        transcript,
        cwd,
        variant,
    })
}

/// 判断 transcript 落在哪个档位的 `projects/` 下。
///
/// 用**小写字符串前缀**而不是 `Path::starts_with`：Windows 路径大小写不敏感，
/// 盘符或目录名的大小写差异会让组件比较误判，而这个判据错了就会把别的文件当会话导出。
/// 比较前把分隔符统一成 `/`，避免 `\` 与 `/` 混用导致前缀不匹配。
fn variant_of_transcript(transcript: &str, roots: &[(WbVariant, PathBuf)]) -> Option<WbVariant> {
    let needle = normalize(transcript);
    for (variant, root) in roots {
        // 注意只拼一次 `projects`：`join` 已经带上它，后面只需补一个分隔符。
        let projects = format!("{}/", normalize(&root.join("projects").to_string_lossy()));
        if needle.starts_with(&projects) {
            return Some(*variant);
        }
    }
    None
}

/// 归一化路径用于前缀比较：分隔符统一为 `/`、去掉 `\\?\` 前缀、转小写。
fn normalize(path: &str) -> String {
    let unified = path.replace('\\', "/");
    let stripped = unified.strip_prefix("//?/").unwrap_or(&unified);
    stripped.to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一个隔离的临时目录树：store 根 + 两个档位数据根。
    fn fixture() -> (tempdir::TempDir, Vec<(WbVariant, PathBuf)>) {
        let tmp = tempdir::TempDir::new();
        let cn = tmp.path().join("wb-cn");
        let ai = tmp.path().join("wb-ai");
        std::fs::create_dir_all(cn.join("projects/ws")).unwrap();
        std::fs::create_dir_all(ai.join("projects/ws")).unwrap();
        let roots = vec![(WbVariant::Cn, cn.clone()), (WbVariant::Ai, ai.clone())];
        (tmp, roots)
    }

    /// 极小临时目录助手，避免为单测引入外部依赖。
    mod tempdir {
        use std::path::{Path, PathBuf};
        use std::sync::atomic::{AtomicU64, Ordering};

        static COUNTER: AtomicU64 = AtomicU64::new(0);

        pub struct TempDir(PathBuf);

        impl TempDir {
            pub fn new() -> Self {
                let n = COUNTER.fetch_add(1, Ordering::Relaxed);
                let dir = std::env::temp_dir().join(format!(
                    "wb-switch-active-session-test-{}-{}",
                    std::process::id(),
                    n
                ));
                let _ = std::fs::remove_dir_all(&dir);
                std::fs::create_dir_all(&dir).expect("创建临时目录");
                Self(dir)
            }

            pub fn path(&self) -> &Path {
                &self.0
            }
        }

        impl Drop for TempDir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }

    fn transcript_in(roots: &[(WbVariant, PathBuf)], variant_index: usize, cid: &str) -> PathBuf {
        roots[variant_index]
            .1
            .join("projects")
            .join("ws")
            .join(format!("{cid}.jsonl"))
    }

    #[test]
    fn records_workbuddy_conversation_by_path() {
        let (tmp, roots) = fixture();
        let transcript = transcript_in(&roots, 0, "sess-1");
        std::fs::write(&transcript, "{}").unwrap();

        let result = record_at(
            tmp.path(),
            &json!({
                "transcript_path": transcript.to_string_lossy(),
                "session_id": "sess-1",
                "cwd": "/work/demo"
            }),
            &roots,
        )
        .expect("应记录成功");

        assert_eq!(result.get("recorded").and_then(Value::as_bool), Some(true));

        let current = current_at(tmp.path()).expect("应有当前对话");
        assert_eq!(current.session_id, "sess-1");
        assert_eq!(current.variant, WbVariant::Cn);
        assert_eq!(current.cwd, "/work/demo");
    }

    /// 国际版目录要判成 ai，不能因为前缀相似就串到国内版。
    #[test]
    fn distinguishes_variants() {
        let (tmp, roots) = fixture();
        let transcript = transcript_in(&roots, 1, "sess-ai");
        std::fs::write(&transcript, "{}").unwrap();

        record_at(
            tmp.path(),
            &json!({ "transcript_path": transcript.to_string_lossy() }),
            &roots,
        )
        .unwrap();

        assert_eq!(current_at(tmp.path()).unwrap().variant, WbVariant::Ai);
    }

    /// 非 WorkBuddy 会话（例如 CodeBuddy CLI 的转录）必须跳过、且不覆盖已有指针。
    #[test]
    fn skips_transcripts_outside_workbuddy_roots() {
        let (tmp, roots) = fixture();
        let transcript = transcript_in(&roots, 0, "keep-me");
        std::fs::write(&transcript, "{}").unwrap();
        record_at(
            tmp.path(),
            &json!({ "transcript_path": transcript.to_string_lossy() }),
            &roots,
        )
        .unwrap();

        let outside = tmp.path().join("elsewhere/other.jsonl");
        let skipped = record_at(
            tmp.path(),
            &json!({ "transcript_path": outside.to_string_lossy() }),
            &roots,
        )
        .unwrap();

        assert_eq!(
            skipped.get("recorded").and_then(Value::as_bool),
            Some(false)
        );
        // 原指针仍在：跳过不等于清空。
        assert_eq!(current_at(tmp.path()).unwrap().session_id, "keep-me");
    }

    #[test]
    fn missing_transcript_path_is_an_error() {
        let (tmp, roots) = fixture();
        assert!(record_at(tmp.path(), &json!({ "session_id": "x" }), &roots).is_err());
    }

    /// payload 没带 session_id 时用文件名主干兜底。
    #[test]
    fn derives_session_id_from_file_stem() {
        let (tmp, roots) = fixture();
        let transcript = transcript_in(&roots, 0, "from-filename");
        std::fs::write(&transcript, "{}").unwrap();

        record_at(
            tmp.path(),
            &json!({ "transcript_path": transcript.to_string_lossy() }),
            &roots,
        )
        .unwrap();

        assert_eq!(current_at(tmp.path()).unwrap().session_id, "from-filename");
    }

    /// 指针指向的文件被删掉后，不应再被当成「当前对话」。
    #[test]
    fn stale_pointer_is_ignored() {
        let (tmp, roots) = fixture();
        let transcript = transcript_in(&roots, 0, "gone");
        std::fs::write(&transcript, "{}").unwrap();
        record_at(
            tmp.path(),
            &json!({ "transcript_path": transcript.to_string_lossy() }),
            &roots,
        )
        .unwrap();
        std::fs::remove_file(&transcript).unwrap();

        assert!(current_at(tmp.path()).is_none());
    }

    /// 小写 / 分隔符混用 / `\\?\` 前缀都不应影响档位判定。
    #[test]
    fn path_matching_is_case_and_separator_insensitive() {
        let normalized = normalize("\\\\?\\C:\\Users\\X\\.workbuddy\\projects\\ws\\a.jsonl");
        assert_eq!(normalized, "c:/users/x/.workbuddy/projects/ws/a.jsonl");
    }

    /// VS Code 那条链路挑的是「最近更新 **且带正文**」的会话。
    ///
    /// 两个点都要守住：没有正文的会话复制过去是空壳；顺序必须自己按 updatedAt 排，
    /// 不能依赖列表顺序（那个顺序由扩展数据目录遍历顺序决定，不是契约）。
    #[test]
    fn vscode_latest_conversation_prefers_newest_with_history() {
        let listed = json!({
            "sessions": [
                { "id": "no-history-newest", "workspaceHash": "ws-a", "updatedAt": 9000, "hasHistory": false },
                { "id": "old-with-history", "workspaceHash": "ws-b", "updatedAt": 1000, "hasHistory": true },
                { "id": "newer-with-history", "workspaceHash": "ws-c", "updatedAt": 5000, "hasHistory": true }
            ]
        });
        let picked = latest_conversation_with_history(&listed).expect("应挑到一条");
        assert_eq!(
            picked.get("id").and_then(Value::as_str),
            Some("newer-with-history")
        );
        assert_eq!(
            picked.get("workspaceHash").and_then(Value::as_str),
            Some("ws-c")
        );
    }

    /// 一条带正文的都没有时返回 None（调用方据此给出明确错误，而不是复制空壳）。
    #[test]
    fn vscode_latest_conversation_is_none_without_history() {
        let listed = json!({
            "sessions": [
                { "id": "a", "workspaceHash": "ws-a", "updatedAt": 5, "hasHistory": false }
            ]
        });
        assert!(latest_conversation_with_history(&listed).is_none());
        assert!(latest_conversation_with_history(&json!({})).is_none());
    }
}
