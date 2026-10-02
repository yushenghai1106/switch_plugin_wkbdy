//! WorkBuddy 客户端档位的单一事实来源。
//!
//! **国际版已下线**：这里只剩国内版。历史上按档位分叉的域名、登录态文件、应用身份、
//! 数据根等取值，现在都收敛为单一取值；`WbVariant` 仍作为「目标客户端」的类型标记保留，
//! 让既有函数签名不必全量改动。
//!
//! 磁盘上的历史国际版数据不动，只在读取账号时用
//! [`WbVariant::is_retired_international`] 把它们排除掉。

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

use crate::modules::config::{
    home_dir, WORKBUDDY_API_ENDPOINT, WORKBUDDY_API_PREFIX, WORKBUDDY_PLATFORM,
};

/// CodeBuddy 系产品域（客户端 `product.json` 的 internalDomain 成员）。
const CN_CODEBUDDY_DOMAIN: &str = "www.codebuddy.cn";

const CN_AUTH_FILE_NAME: &str = "workbuddy-desktop.info";
const CN_DATA_ROOT: &str = ".workbuddy";

const CN_WINDOWS_IMAGES: [&str; 1] = ["WorkBuddy"];
const CN_MACOS_APPS: [&str; 1] = ["WorkBuddy.app"];
const CN_MACOS_BUNDLE_ID: &str = "com.tencent.workbuddy.mac";
const CN_LINUX_APP: &str = "/usr/bin/workbuddy";

/// 已下线的国际版域的判据后缀（只认后缀，避免相似域名被误判）。
const RETIRED_INTL_DOMAIN_SUFFIX: &str = ".workbuddy.ai";

/// 目标平台。登录态文件目录按平台分支；显式取值便于单测覆盖三平台。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // 非当前平台的变体只在本模块单测中构造
pub(crate) enum HostOs {
    Macos,
    Windows,
    Linux,
}

impl HostOs {
    fn current() -> Self {
        #[cfg(target_os = "macos")]
        return Self::Macos;
        #[cfg(target_os = "windows")]
        return Self::Windows;
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        return Self::Linux;
    }
}

/// 注入 CodeBuddy 系目标（VS Code 扩展 / IDE 插件）前的产品域规范化。
///
/// WorkBuddy 客户端首次登录写下的 `www.workbuddy.cn` 不在 CodeBuddy 客户端
/// `product.json` 的 internalDomain 列表里，扩展会把它归类为 `selfhosted`——该分支会读取
/// 「企业版端点」设置（`enterpriseEndpoint`），用户一旦配置就会把请求打到那个端点。这里只
/// 映射已知产品域；其它域（企业自建 / iOA / cloudHosted / 已是 codebuddy 域）原样透传；
/// 空域补产品默认域。
pub fn codebuddy_domain_for(domain: &str, _variant: WbVariant) -> String {
    let trimmed = domain.trim();
    if trimmed.is_empty() {
        return CN_CODEBUDDY_DOMAIN.to_string();
    }
    match trimmed.to_ascii_lowercase().as_str() {
        "www.workbuddy.cn" | "workbuddy.cn" => CN_CODEBUDDY_DOMAIN.to_string(),
        _ => trimmed.to_string(),
    }
}

/// WorkBuddy 客户端档位。
///
/// 国际版已下线，只剩国内版；保留枚举是为了让贯穿全库的函数签名保持不变。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WbVariant {
    /// 国内版。
    Cn,
}

impl WbVariant {
    /// 全部档位（遍历入口，如按档位轮询/展示）。
    pub const ALL: [WbVariant; 1] = [WbVariant::Cn];

    /// 档位名。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cn => "cn",
        }
    }

    /// 解析档位字符串。国际版已下线：任何取值（含历史 `ai` / `intl` / `global`）都回落国内版。
    pub fn parse(_raw: Option<&str>) -> Self {
        Self::Cn
    }

    /// 从账号记录判定档位。国际版已下线，一律国内版。
    pub fn from_account(_acc: &Value) -> Self {
        Self::Cn
    }

    /// 该域名是否属于已下线的国际版（只认后缀，避免相似域名误判）。
    pub fn is_retired_international_domain(domain: &str) -> bool {
        domain
            .trim()
            .to_ascii_lowercase()
            .ends_with(RETIRED_INTL_DOMAIN_SUFFIX)
    }

    /// 该账号记录是否属于**已下线的国际版**（用于读取时过滤历史数据）。
    ///
    /// 判据与下线前一致：显式 `variant` 字段优先，其次 `domain` 后缀。磁盘数据保持不动，
    /// 只在读取账号时把这些条目排除掉。
    pub fn is_retired_international(acc: &Value) -> bool {
        if let Some(raw) = acc.get("variant").and_then(Value::as_str).map(str::trim) {
            if !raw.is_empty() {
                return matches!(raw.to_ascii_lowercase().as_str(), "ai" | "intl" | "global");
            }
        }
        acc.get("domain")
            .and_then(Value::as_str)
            .map(Self::is_retired_international_domain)
            .unwrap_or(false)
    }

    // -----------------------------------------------------------------------
    // 网络
    // -----------------------------------------------------------------------

    /// API 基址。
    pub fn api_endpoint(self) -> &'static str {
        match self {
            Self::Cn => WORKBUDDY_API_ENDPOINT,
        }
    }

    /// 设备码流程接口前缀。
    pub fn api_prefix(self) -> &'static str {
        WORKBUDDY_API_PREFIX
    }

    /// CodeBuddy 系目标的规范产品域（注入会话时使用）。
    pub fn codebuddy_domain(self) -> &'static str {
        match self {
            Self::Cn => CN_CODEBUDDY_DOMAIN,
        }
    }

    /// OAuth `platform` 参数。
    pub fn oauth_platform(self) -> &'static str {
        match self {
            Self::Cn => WORKBUDDY_PLATFORM,
        }
    }

    /// billing 接口路径候选（顺序即回落顺序）。
    ///
    /// 下线前的国际版需要 `/billing/meter/...` → `/v2/billing/meter/...` 的 404 回落；
    /// 该档位移除后只剩国内版的单一候选，与改造前逐字一致。
    pub fn billing_paths(self, path: &str) -> Vec<String> {
        match self {
            Self::Cn => vec![path.to_string()],
        }
    }

    // -----------------------------------------------------------------------
    // 本地
    // -----------------------------------------------------------------------

    /// 官方登录态文件路径。
    pub fn auth_file_path(self) -> PathBuf {
        self.auth_file_path_at(&home_dir(), HostOs::current())
    }

    fn auth_file_path_at(self, home: &Path, os: HostOs) -> PathBuf {
        let dir = match os {
            HostOs::Macos => "Library/Application Support/CodeBuddyExtension/Data/Public/auth",
            HostOs::Windows => "AppData/Local/CodeBuddyExtension/Data/Public/auth",
            HostOs::Linux => ".local/share/CodeBuddyExtension/Data/Public/auth",
        };
        home.join(dir).join(self.auth_file_name())
    }

    /// 登录态文件名。
    fn auth_file_name(self) -> &'static str {
        match self {
            Self::Cn => CN_AUTH_FILE_NAME,
        }
    }

    /// 切换前备份文件名。
    pub fn backup_file_name(self, ts: &str) -> String {
        let stem = self.auth_file_name().trim_end_matches(".info");
        format!("{stem}.{ts}.info")
    }

    /// 客户端数据根。
    pub fn data_root(self) -> PathBuf {
        home_dir().join(match self {
            Self::Cn => CN_DATA_ROOT,
        })
    }

    /// exe 路径缓存的分键（与档位名同形，独立方法便于将来单独演进盘上格式）。
    pub fn exe_cache_key(self) -> &'static str {
        self.as_str()
    }

    // -----------------------------------------------------------------------
    // 应用身份
    // -----------------------------------------------------------------------

    /// Windows 映像名（精确等价匹配，忽略 `.exe` 与大小写）。
    pub fn windows_image_names(self) -> &'static [&'static str] {
        match self {
            Self::Cn => &CN_WINDOWS_IMAGES,
        }
    }

    /// macOS app 名（候选目录与进程模式的主名）。
    pub fn macos_app_names(self) -> &'static [&'static str] {
        match self {
            Self::Cn => &CN_MACOS_APPS,
        }
    }

    /// macOS 优雅退出用的 bundle id。
    pub fn macos_bundle_id(self) -> &'static str {
        match self {
            Self::Cn => CN_MACOS_BUNDLE_ID,
        }
    }

    /// macOS app 路径探测全失败时的回落路径。
    pub fn macos_default_app_path(self) -> PathBuf {
        Path::new("/Applications").join(self.macos_app_names()[0])
    }

    /// Linux 默认可执行文件路径。
    pub fn linux_app_path(self) -> PathBuf {
        PathBuf::from(match self {
            Self::Cn => CN_LINUX_APP,
        })
    }

    /// Linux 进程查询模式。
    pub fn linux_process_pattern(self) -> &'static str {
        match self {
            Self::Cn => "workbuddy",
        }
    }

    // -----------------------------------------------------------------------
    // 能力声明
    // -----------------------------------------------------------------------

    /// 成长中心（派猫猫旅行）是否开放。国际版下线后恒为开放。
    pub fn supports_travel(self) -> bool {
        matches!(self, Self::Cn)
    }

    /// 该档位是否支持签到，**同时门控请求与待签到集合**。
    pub fn supports_checkin(self) -> bool {
        matches!(self, Self::Cn)
    }

    /// 是否支持切换时复制会话（能力探测，见 `session::session_copy_supported_at`）。
    ///
    /// 不按档位写死：必须探测数据根的实际能力（design D6）。
    pub fn supports_session_copy(self) -> bool {
        crate::modules::session::session_copy_supported_at(&self.data_root())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 国际版下线后：任何输入都解析为国内版（含历史 `ai` / `intl` / `global`）。
    #[test]
    fn parse_always_falls_back_to_cn() {
        for raw in [
            None,
            Some(""),
            Some("   "),
            Some("cn"),
            Some(" CN "),
            Some("ai"),
            Some("intl"),
            Some("global"),
            Some("unknown"),
        ] {
            assert_eq!(WbVariant::parse(raw), WbVariant::Cn, "输入 {raw:?}");
        }
    }

    #[test]
    fn from_account_is_always_cn() {
        assert_eq!(WbVariant::from_account(&json!({})), WbVariant::Cn);
        assert_eq!(
            WbVariant::from_account(&json!({"variant": "ai"})),
            WbVariant::Cn
        );
        assert_eq!(
            WbVariant::from_account(&json!({"domain": "www.workbuddy.ai"})),
            WbVariant::Cn
        );
    }

    /// 历史国际版账号必须能被认出来（读取时过滤，磁盘数据不动）。
    #[test]
    fn retired_international_detection() {
        // 显式字段优先。
        assert!(WbVariant::is_retired_international(
            &json!({"variant": "ai"})
        ));
        assert!(WbVariant::is_retired_international(
            &json!({"variant": " AI "})
        ));
        assert!(WbVariant::is_retired_international(
            &json!({"variant": "intl"})
        ));
        assert!(WbVariant::is_retired_international(
            &json!({"variant": "global"})
        ));
        // 域名后缀兜底。
        assert!(WbVariant::is_retired_international(
            &json!({"domain": "www.workbuddy.ai"})
        ));
        // 只认后缀，不允许相似域名误判。
        assert!(!WbVariant::is_retired_international(
            &json!({"domain": "www.workbuddy.ai.evil.com"})
        ));
        // 显式国内版字段优先于域名。
        assert!(!WbVariant::is_retired_international(
            &json!({"variant": "cn", "domain": "www.workbuddy.ai"})
        ));
        // 国内版账号与空记录都不算。
        assert!(!WbVariant::is_retired_international(&json!({})));
        assert!(!WbVariant::is_retired_international(
            &json!({"domain": "www.codebuddy.cn"})
        ));
    }

    #[test]
    fn serde_round_trip_uses_lowercase_names() {
        assert_eq!(serde_json::to_string(&WbVariant::Cn).unwrap(), "\"cn\"");
        assert_eq!(
            serde_json::from_str::<WbVariant>("\"cn\"").unwrap(),
            WbVariant::Cn
        );
        assert_eq!(WbVariant::ALL.map(|v| v.as_str()), ["cn"]);
    }

    #[test]
    fn auth_file_path_covers_three_platforms() {
        let home = Path::new("/home/tester");
        // 比较 Path 而非 to_string_lossy：Windows 的 Path::join 产出 `\` 分隔符，
        // 写死正斜杠的字符串断言会在 Windows 上失败——分隔符不是被测行为的一部分。
        let cn = WbVariant::Cn.auth_file_path_at(home, HostOs::Macos);
        assert_eq!(
            cn,
            home.join("Library/Application Support/CodeBuddyExtension/Data/Public/auth")
                .join("workbuddy-desktop.info")
        );
        let cn_win = WbVariant::Cn.auth_file_path_at(home, HostOs::Windows);
        assert_eq!(
            cn_win,
            home.join("AppData/Local/CodeBuddyExtension/Data/Public/auth")
                .join("workbuddy-desktop.info")
        );
        let cn_linux = WbVariant::Cn.auth_file_path_at(home, HostOs::Linux);
        assert_eq!(
            cn_linux,
            home.join(".local/share/CodeBuddyExtension/Data/Public/auth")
                .join("workbuddy-desktop.info")
        );
    }

    /// 回归：路径与本模块改造前逐字一致。
    #[test]
    fn current_platform_auth_path_keeps_cn_default() {
        #[cfg(target_os = "macos")]
        let legacy = home_dir()
            .join("Library/Application Support/CodeBuddyExtension/Data/Public/auth/workbuddy-desktop.info");
        #[cfg(target_os = "windows")]
        let legacy = home_dir()
            .join("AppData/Local/CodeBuddyExtension/Data/Public/auth/workbuddy-desktop.info");
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        let legacy = home_dir()
            .join(".local/share/CodeBuddyExtension/Data/Public/auth/workbuddy-desktop.info");

        assert_eq!(WbVariant::Cn.auth_file_path(), legacy);
    }

    #[test]
    fn backup_file_names_use_cn_stem() {
        assert_eq!(
            WbVariant::Cn.backup_file_name("2026-09-15T00-00-00Z"),
            "workbuddy-desktop.2026-09-15T00-00-00Z.info"
        );
    }

    #[test]
    fn data_root_and_cache_keys() {
        assert!(WbVariant::Cn.data_root().ends_with(CN_DATA_ROOT));
        assert_eq!(WbVariant::Cn.exe_cache_key(), "cn");
    }

    #[test]
    fn network_and_identity() {
        assert_eq!(WbVariant::Cn.api_endpoint(), "https://www.codebuddy.cn");
        assert_eq!(WbVariant::Cn.api_prefix(), "/v2/plugin");
        assert_eq!(WbVariant::Cn.oauth_platform(), "workbuddy");

        assert_eq!(
            WbVariant::Cn.windows_image_names(),
            ["WorkBuddy"].as_slice()
        );
        assert_eq!(
            WbVariant::Cn.macos_app_names(),
            ["WorkBuddy.app"].as_slice()
        );
        assert_eq!(WbVariant::Cn.macos_bundle_id(), "com.tencent.workbuddy.mac");
        // 断言父目录与文件名，而不是整串字符串：Windows 上 join 产出 `\`。
        let cn_app = WbVariant::Cn.macos_default_app_path();
        assert_eq!(cn_app.parent(), Some(Path::new("/Applications")));
        assert_eq!(cn_app.file_name().unwrap(), "WorkBuddy.app");
    }

    #[test]
    fn capability_declarations() {
        assert!(WbVariant::Cn.supports_travel());
        assert!(WbVariant::Cn.supports_checkin());
    }

    #[test]
    fn billing_paths_keep_cn_single_candidate() {
        assert_eq!(
            WbVariant::Cn.billing_paths("/v2/billing/meter/daily-checkin"),
            vec!["/v2/billing/meter/daily-checkin"]
        );
        assert_eq!(
            WbVariant::Cn.billing_paths("/billing/meter/get-user-resource-summary"),
            vec!["/billing/meter/get-user-resource-summary"]
        );
    }

    #[test]
    fn codebuddy_domain_normalizes_workbuddy_product_domains() {
        assert_eq!(
            codebuddy_domain_for("www.workbuddy.cn", WbVariant::Cn),
            "www.codebuddy.cn"
        );
        assert_eq!(
            codebuddy_domain_for("workbuddy.cn", WbVariant::Cn),
            "www.codebuddy.cn"
        );

        // 已是 CodeBuddy 域 / 企业自建域 / iOA 域 → 原样透传（不改客户私有部署）。
        assert_eq!(
            codebuddy_domain_for("www.codebuddy.cn", WbVariant::Cn),
            "www.codebuddy.cn"
        );
        assert_eq!(
            codebuddy_domain_for("corp.example.com", WbVariant::Cn),
            "corp.example.com"
        );
        assert_eq!(
            codebuddy_domain_for("tencent.sso.codebuddy.cn", WbVariant::Cn),
            "tencent.sso.codebuddy.cn"
        );

        // 空域 / 纯空白 → 补产品默认域。
        assert_eq!(codebuddy_domain_for("", WbVariant::Cn), "www.codebuddy.cn");
        assert_eq!(
            codebuddy_domain_for("   ", WbVariant::Cn),
            "www.codebuddy.cn"
        );
    }
}
