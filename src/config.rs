//! TOML 設定ファイル + `@alias` 起動 (issue #46)。
//!
//! ## v1 スコープ
//!
//! - `~/.config/vozltop/config.toml` (XDG) を読み込み、`vozltop @prod` 形式の
//!   `@alias` 引数を `[hosts.prod]` セクションに解決する
//! - `[hosts.<alias>]` に書ける項目: `url` / `user` / `interval` / `headers` /
//!   `insecure` / `no_color` / `alert_5xx_pct` / `alert_p95_ms`
//! - `[defaults]` セクションで全 host 共通のデフォルトを設定可
//! - 優先度: CLI フラグ > `[hosts.<alias>]` > `[defaults]` > 組み込み既定
//!
//! ## v1 範囲外 (別 issue に倒す)
//!
//! - **keyring 連携**: password は本 PR では config に平文で書く。
//!   `chmod 0600 ~/.config/vozltop/config.toml` を推奨。
//! - **alias 以外での config 参照**: 通常の URL 直指定 (`vozltop https://...`)
//!   の場合、CLI で明示されなかったフィールドの config フォールバックは行わない。
//!   alias 経由のときのみ config が効く。これは clap の "CLI 既定値" と
//!   "config 由来の値" を区別する仕組みが標準にないため、最小実装に倒した結果。
//!   将来必要になったら別 issue で `ArgMatches::value_source` ベースに拡張する。
//! - **`vozltop config edit` 等の subcommand**: config は手動編集
//!
//! ## TOML スキーマ例
//!
//! ```toml
//! [defaults]
//! interval = 1.0
//! no_color = false
//!
//! [hosts.prod]
//! url = "https://nginx.prod.example.com/status/format/json"
//! user = "admin:secret"  # ※ 平文許容、chmod 0600 推奨
//! interval = 0.5
//! alert_5xx_pct = 1.0
//!
//! [hosts.staging]
//! url = "https://nginx.staging.example.com/status/format/json"
//! ```
//!
//! ## config path の探索順
//!
//! 1. `--config <path>` 明示指定 (本 PR で追加するフラグ)
//! 2. `$VOZLTOP_CONFIG` 環境変数
//! 3. `directories::ProjectDirs::config_dir() / "config.toml"`
//!    (Linux: `~/.config/vozltop/config.toml`,
//!    macOS: `~/Library/Application Support/vozltop/config.toml`)
//!
//! 1-3 のいずれも存在しない場合は空 config として動作 (= 従来通り CLI のみで動く)。
//! `@alias` 引数を渡したのに config が無い / alias が定義されていない場合のみエラー。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// 環境変数で config path を明示指定する場合のキー。
pub const CONFIG_ENV_VAR: &str = "VOZLTOP_CONFIG";

/// `@alias` 引数の prefix。
pub const ALIAS_PREFIX: char = '@';

/// `directories` crate に渡す qualifier / organization / application 文字列。
///
/// Linux では `~/.config/vozltop/`、macOS では
/// `~/Library/Application Support/vozltop/`、Windows では `%APPDATA%\\vozltop\\`
/// が config_dir() の結果になる。`qualifier` (com.example) は Linux では無視され、
/// macOS で `bundle identifier` 風の reverse-DNS に使われる。空文字列でも動く。
const APP_QUALIFIER: &str = "";
const APP_ORG: &str = "";
const APP_NAME: &str = "vozltop";

/// TOML config 全体のルート。
///
/// 全フィールドが `Option` / `#[serde(default)]` なので、空ファイルや存在しない
/// セクションでも decode は成功する。
#[derive(Deserialize, Debug, Default, Clone)]
pub struct Config {
    /// 全 host 共通のデフォルト値。
    #[serde(default)]
    pub defaults: Defaults,
    /// `vozltop @<alias>` で参照される host 別設定。キー = alias 名。
    #[serde(default)]
    pub hosts: HashMap<String, HostConfig>,
}

/// `[defaults]` セクション。すべて任意。
#[derive(Deserialize, Debug, Default, Clone)]
pub struct Defaults {
    pub interval: Option<f64>,
    pub user: Option<String>,
    pub headers: Option<Vec<String>>,
    pub insecure: Option<bool>,
    pub no_color: Option<bool>,
    pub alert_5xx_pct: Option<f64>,
    pub alert_p95_ms: Option<u64>,
}

/// `[hosts.<alias>]` セクション。`url` のみ必須、他は任意。
#[derive(Deserialize, Debug, Clone)]
pub struct HostConfig {
    /// nginx-vts の status endpoint URL。
    pub url: String,
    pub interval: Option<f64>,
    pub user: Option<String>,
    pub headers: Option<Vec<String>>,
    pub insecure: Option<bool>,
    pub no_color: Option<bool>,
    pub alert_5xx_pct: Option<f64>,
    pub alert_p95_ms: Option<u64>,
}

impl Config {
    /// `@alias` 形式の引数なら `Some(alias_name)` を返す。それ以外は `None`。
    ///
    /// 引数が空 / `@` だけ / 通常の URL なら `None`。`@a@b` のように途中に `@` が
    /// 含まれる場合は `Some("a@b")` を返す (alias 名に `@` を含めるユースケースは
    /// 想定外だが parser としては防御的に許容)。
    pub fn parse_alias_arg(arg: &str) -> Option<&str> {
        arg.strip_prefix(ALIAS_PREFIX).filter(|s| !s.is_empty())
    }

    /// 解決済み host 設定を返す。alias が config に無ければ `Err`。
    pub fn resolve_alias(&self, alias: &str) -> Result<&HostConfig, ConfigError> {
        self.hosts
            .get(alias)
            .ok_or_else(|| ConfigError::UnknownAlias(alias.to_string()))
    }

    /// config を実ファイルから読み込む。`path` が `None` なら探索順に従う。
    ///
    /// 存在しないファイルは空 config として扱う (`Ok(Config::default())`)。
    /// `@alias` 使用時のみ呼び出し側で `resolve_alias` を呼んで未定義検出する。
    pub fn load(explicit_path: Option<&Path>) -> Result<Self, ConfigError> {
        let path = match explicit_path {
            Some(p) => Some(p.to_path_buf()),
            None => env_config_path().or_else(default_config_path),
        };
        match path {
            Some(p) if p.exists() => {
                let text = std::fs::read_to_string(&p).map_err(|e| ConfigError::Io {
                    path: p.clone(),
                    source: e,
                })?;
                Self::parse(&text).map_err(|e| ConfigError::Parse {
                    path: p.clone(),
                    source: e,
                })
            }
            // explicit path が指定されたのに存在しない場合はエラーにしたい。
            Some(p) if explicit_path.is_some() => Err(ConfigError::Io {
                path: p.clone(),
                source: std::io::Error::from(std::io::ErrorKind::NotFound),
            }),
            // path 未解決 or 探索パスに無い → 空 config (= 従来動作)
            _ => Ok(Self::default()),
        }
    }

    /// 文字列から直接パースする (テスト用)。
    pub fn parse(text: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(text)
    }
}

/// `$VOZLTOP_CONFIG` を読む。空文字 / 未設定なら `None`。
fn env_config_path() -> Option<PathBuf> {
    std::env::var_os(CONFIG_ENV_VAR)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// `directories::ProjectDirs::config_dir() / "config.toml"` を返す。
///
/// `ProjectDirs::from` が `None` を返すのは HOME が取れない極端な環境
/// (sandbox 等)。その場合 `None` を返して config なしで動く。
fn default_config_path() -> Option<PathBuf> {
    directories::ProjectDirs::from(APP_QUALIFIER, APP_ORG, APP_NAME)
        .map(|dirs| dirs.config_dir().join("config.toml"))
}

/// config 関連エラー。
#[derive(Debug)]
pub enum ConfigError {
    /// ファイル I/O 失敗 (open / read)。
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    /// TOML パース失敗。
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    /// `@alias` が config の `[hosts.*]` に見つからない。
    UnknownAlias(String),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::Io { path, source } => {
                write!(f, "config file {}: {source}", path.display())
            }
            ConfigError::Parse { path, source } => {
                write!(f, "config file {}: parse error: {source}", path.display())
            }
            ConfigError::UnknownAlias(name) => {
                write!(
                    f,
                    "unknown alias @{name}: not found in config [hosts.*] sections"
                )
            }
        }
    }
}

impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ConfigError::Io { source, .. } => Some(source),
            ConfigError::Parse { source, .. } => Some(source),
            ConfigError::UnknownAlias(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_alias_arg_strips_at_prefix() {
        assert_eq!(Config::parse_alias_arg("@prod"), Some("prod"));
        assert_eq!(Config::parse_alias_arg("@staging-2"), Some("staging-2"));
    }

    #[test]
    fn parse_alias_arg_returns_none_for_non_alias() {
        assert_eq!(Config::parse_alias_arg(""), None);
        assert_eq!(Config::parse_alias_arg("https://example.com/status"), None);
        assert_eq!(Config::parse_alias_arg("@"), None, "bare @ is not an alias");
    }

    #[test]
    fn empty_string_parses_to_default_config() {
        let c = Config::parse("").unwrap();
        assert!(c.hosts.is_empty());
        assert!(c.defaults.interval.is_none());
    }

    #[test]
    fn parses_full_schema_example() {
        let toml = r#"
[defaults]
interval = 2.0
no_color = true
alert_5xx_pct = 1.5

[hosts.prod]
url = "https://nginx.prod.example.com/status/format/json"
user = "admin:secret"
interval = 0.5

[hosts.staging]
url = "https://nginx.staging.example.com/status/format/json"
"#;
        let c = Config::parse(toml).unwrap();
        assert_eq!(c.defaults.interval, Some(2.0));
        assert_eq!(c.defaults.no_color, Some(true));
        assert_eq!(c.defaults.alert_5xx_pct, Some(1.5));
        assert_eq!(c.hosts.len(), 2);
        let prod = c.hosts.get("prod").unwrap();
        assert_eq!(
            prod.url,
            "https://nginx.prod.example.com/status/format/json"
        );
        assert_eq!(prod.user.as_deref(), Some("admin:secret"));
        assert_eq!(prod.interval, Some(0.5));
        let staging = c.hosts.get("staging").unwrap();
        assert!(staging.user.is_none());
    }

    #[test]
    fn resolve_alias_returns_host_or_error() {
        let c = Config::parse("[hosts.prod]\nurl = \"https://prod/\"").unwrap();
        assert_eq!(c.resolve_alias("prod").unwrap().url, "https://prod/");
        let err = c.resolve_alias("missing").unwrap_err();
        assert!(matches!(err, ConfigError::UnknownAlias(name) if name == "missing"));
    }

    #[test]
    fn invalid_toml_returns_parse_error() {
        let err = Config::parse("this is not [toml").unwrap_err();
        let msg = err.to_string();
        assert!(!msg.is_empty(), "parse error should have a message");
    }

    #[test]
    fn unknown_alias_error_message_is_helpful() {
        let err = ConfigError::UnknownAlias("typo".to_string());
        let msg = err.to_string();
        assert!(msg.contains("typo"), "msg: {msg}");
        assert!(
            msg.contains("[hosts.*]"),
            "msg should mention config: {msg}"
        );
    }
}
