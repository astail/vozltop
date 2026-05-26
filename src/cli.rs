//! CLI 引数の `clap` derive 定義と value_parser。
//!
//! issue #18 で raw 文字列を保持する暫定 `Args` を導入し、issue #34 で
//! `#[derive(clap::Parser)]` + `value_parser = ...` に置き換えた。
//!
//! 旧版は `VtsClient::new` が `cli::parse_user` / `cli::parse_header` を
//! 呼び直す形だったが、本 PR から **CLI パース時に検証 + 正規化** する。
//! その結果:
//!
//! - `Args.user` は `Option<(String, String)>`
//! - `Args.headers` は `Vec<(HeaderName, HeaderValue)>`
//! - 不正値はクラップが起動時に止め、`VtsClient` 構築は infallible に近づく
//!
//! `parse_user` / `parse_header` / `parse_interval` を `pub` で残しているのは
//! 単体テスト + 将来 issue #40 で `VOZLTOP_PASSWORD` 環境変数フォールバックを
//! 入れる際にも同じ検証器を再利用するため。

use std::env;
use std::str::FromStr;

use clap::Parser;
use reqwest::header::{HeaderName, HeaderValue};
use url::Url;

/// 受理する `--interval` の下限 (秒)。
///
/// 0.1 未満は (1) tokio タイマーの解像度 (2) nginx 側への過剰負荷
/// (3) ratatui の描画コスト の 3 点で危険なので一律に弾く。
pub const MIN_INTERVAL_SECS: f64 = 0.1;

/// 受理する `--interval` の上限 (秒)。
///
/// 60 秒を超えると derived RPS の分母が大きすぎてリアルタイム監視として
/// 意味を失う。長期トレンド向けは Phase 2 の別機能 (sparkline 拡張) で扱う。
pub const MAX_INTERVAL_SECS: f64 = 60.0;

/// CLI 引数。`vozltop --help` の出力もここから生成される。
#[derive(Parser, Debug, Clone)]
#[command(
    version,
    about = "htop-like real-time TUI for nginx-module-vts",
    long_about = None,
)]
pub struct Args {
    /// nginx-vts の `/status/format/json` などを指す絶対 URL。
    #[arg(value_name = "URL")]
    pub url: Url,

    /// リフレッシュ間隔 (秒)。
    #[arg(
        short,
        long,
        default_value_t = 1.0,
        value_name = "SECONDS",
        value_parser = parse_interval,
    )]
    pub interval: f64,

    /// HTTP Basic 認証用の `user:pass`。
    #[arg(
        short = 'u',
        long,
        value_name = "USER:PASS",
        value_parser = parse_user,
    )]
    pub user: Option<(String, String)>,

    /// 追加ヘッダ (`K: V` 形式、繰り返し可)。
    #[arg(
        short = 'H',
        long = "header",
        value_name = "K: V",
        value_parser = parse_header,
    )]
    pub headers: Vec<(HeaderName, HeaderValue)>,

    /// TLS 証明書検証を無効化する。
    #[arg(long)]
    pub insecure: bool,

    /// 色を無効化する (環境変数 `NO_COLOR` でも同等)。
    #[arg(long = "no-color")]
    pub no_color: bool,
}

impl Args {
    /// `--no-color` フラグと `NO_COLOR` 環境変数の OR 評価。
    ///
    /// <https://no-color.org/> 準拠: `NO_COLOR` は **値の有無に関わらず非空で
    /// セットされていれば真**。空文字列のみ「未指定」と同等扱い。
    pub fn no_color_effective(&self) -> bool {
        self.no_color || no_color_env()
    }
}

/// `NO_COLOR` 環境変数の判定 (https://no-color.org/)。
pub fn no_color_env() -> bool {
    matches!(env::var_os("NO_COLOR"), Some(v) if !v.is_empty())
}

/// `--interval` の値パーサ。
///
/// - 数値としてパースできない → エラー
/// - NaN / Inf → エラー
/// - 範囲 `MIN_INTERVAL_SECS..=MAX_INTERVAL_SECS` 外 → エラー
pub fn parse_interval(s: &str) -> Result<f64, String> {
    let v: f64 = s.parse().map_err(|_| format!("`{s}` is not a number"))?;
    if !v.is_finite() {
        return Err(format!("interval must be finite (got {s:?})"));
    }
    if !(MIN_INTERVAL_SECS..=MAX_INTERVAL_SECS).contains(&v) {
        return Err(format!(
            "interval must be between {MIN_INTERVAL_SECS} and {MAX_INTERVAL_SECS} seconds (got {v})"
        ));
    }
    Ok(v)
}

/// `--user user:pass` 形式を `(user, password)` に分解する。
///
/// - 最初のコロンで 1 回だけ split (RFC 7617 では password 側のコロンは許容)
/// - username が空はエラー (RFC 7617 で禁止)
/// - password が空は OK (curl の挙動に合わせる)
pub fn parse_user(s: &str) -> Result<(String, String), String> {
    let (user, pass) = s
        .split_once(':')
        .ok_or_else(|| format!("--user must be in `user:pass` format (got {s:?})"))?;
    if user.is_empty() {
        return Err(format!("--user: username must not be empty (got {s:?})"));
    }
    Ok((user.to_string(), pass.to_string()))
}

/// `--header 'K: V'` 形式を `(HeaderName, HeaderValue)` に分解する。
///
/// - 最初のコロンで 1 回だけ split (value 側のコロンは許容: `Bearer xyz:abc` など)
/// - name の前後の空白は trim する
/// - value の前後の OWS (= 0 個以上の SP/HTAB) を RFC 9110 §5.5 に従って除去する
/// - `HeaderName::from_str` / `HeaderValue::from_str` の検証に委譲
///   (CR/LF などのヘッダインジェクションは reqwest 側で弾かれる)
pub fn parse_header(s: &str) -> Result<(HeaderName, HeaderValue), String> {
    let (name, value) = s
        .split_once(':')
        .ok_or_else(|| format!("--header must be in `K: V` format (got {s:?})"))?;
    let name = name.trim();
    if name.is_empty() {
        return Err(format!("--header: name must not be empty (got {s:?})"));
    }
    // RFC 9110 §5.5: OWS = *( SP / HTAB ). value の前後の OWS は意味を持たない。
    let value = value.trim_matches(|c: char| c == ' ' || c == '\t');
    let name =
        HeaderName::from_str(name).map_err(|e| format!("--header: invalid name {name:?}: {e}"))?;
    let value = HeaderValue::from_str(value)
        .map_err(|e| format!("--header: invalid value {value:?}: {e}"))?;
    Ok((name, value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    // ---------- parse_user ----------

    #[test]
    fn parse_user_accepts_basic_form() {
        let (u, p) = parse_user("alice:secret").unwrap();
        assert_eq!(u, "alice");
        assert_eq!(p, "secret");
    }

    #[test]
    fn parse_user_allows_colon_in_password() {
        // RFC 7617: username は colon 禁止だが password は colon 含んで OK
        let (u, p) = parse_user("alice:s:e:c:r:e:t").unwrap();
        assert_eq!(u, "alice");
        assert_eq!(p, "s:e:c:r:e:t");
    }

    #[test]
    fn parse_user_allows_empty_password() {
        let (u, p) = parse_user("alice:").unwrap();
        assert_eq!(u, "alice");
        assert_eq!(p, "");
    }

    #[test]
    fn parse_user_rejects_no_colon() {
        assert!(parse_user("alice").is_err());
    }

    #[test]
    fn parse_user_rejects_empty_username() {
        assert!(parse_user(":secret").is_err());
        assert!(parse_user("").is_err());
    }

    // ---------- parse_header ----------

    #[test]
    fn parse_header_accepts_basic_form() {
        let (n, v) = parse_header("Authorization: Bearer xyz").unwrap();
        assert_eq!(n.as_str(), "authorization");
        assert_eq!(v.to_str().unwrap(), "Bearer xyz");
    }

    #[test]
    fn parse_header_allows_colon_in_value() {
        let (_, v) = parse_header("X-Token: foo:bar:baz").unwrap();
        assert_eq!(v.to_str().unwrap(), "foo:bar:baz");
    }

    #[test]
    fn parse_header_accepts_no_space_after_colon() {
        let (_, v) = parse_header("X-Foo:bar").unwrap();
        assert_eq!(v.to_str().unwrap(), "bar");
    }

    #[test]
    fn parse_header_rejects_no_colon() {
        assert!(parse_header("invalid").is_err());
    }

    #[test]
    fn parse_header_rejects_empty_name() {
        assert!(parse_header(": only-value").is_err());
        assert!(parse_header(":").is_err());
    }

    #[test]
    fn parse_header_rejects_invalid_name() {
        // スペース入りの header name は HTTP 仕様違反
        assert!(parse_header("Bad Name: value").is_err());
    }

    #[test]
    fn parse_header_rejects_crlf_injection() {
        // CR/LF を含む値は reqwest::header::HeaderValue 側で弾かれる
        assert!(parse_header("X-Foo: bar\r\nEvil: yes").is_err());
    }

    #[test]
    fn parse_header_trims_multiple_leading_and_trailing_ows() {
        // RFC 9110 OWS 除去: SP/HTAB が複数あっても全て削る
        let (_, v) = parse_header("X-Foo:   bar  ").unwrap();
        assert_eq!(v.to_str().unwrap(), "bar");
        let (_, v) = parse_header("X-Foo:\t\t bar\t").unwrap();
        assert_eq!(v.to_str().unwrap(), "bar");
    }

    // ---------- parse_interval ----------

    #[test]
    fn parse_interval_accepts_default() {
        assert_eq!(parse_interval("1.0").unwrap(), 1.0);
    }

    #[test]
    fn parse_interval_accepts_boundaries() {
        assert_eq!(parse_interval("0.1").unwrap(), 0.1);
        assert_eq!(parse_interval("60").unwrap(), 60.0);
    }

    #[test]
    fn parse_interval_rejects_below_min() {
        let err = parse_interval("0.05").unwrap_err();
        assert!(err.contains("between"), "expected range error: {err}");
    }

    #[test]
    fn parse_interval_rejects_above_max() {
        assert!(parse_interval("60.001").is_err());
        assert!(parse_interval("3600").is_err());
    }

    #[test]
    fn parse_interval_rejects_nan_and_inf() {
        assert!(parse_interval("NaN").is_err());
        assert!(parse_interval("inf").is_err());
    }

    #[test]
    fn parse_interval_rejects_non_number() {
        assert!(parse_interval("abc").is_err());
        assert!(parse_interval("").is_err());
    }

    // ---------- Args::try_parse_from ----------

    fn try_parse(args: &[&str]) -> Result<Args, clap::Error> {
        Args::try_parse_from(args)
    }

    #[test]
    fn args_requires_url() {
        let err = try_parse(&["vozltop"]).unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::MissingRequiredArgument);
    }

    #[test]
    fn args_default_interval_is_1() {
        let a = try_parse(&["vozltop", "http://example.com/status"]).unwrap();
        assert_eq!(a.interval, 1.0);
        assert!(a.user.is_none());
        assert!(a.headers.is_empty());
        assert!(!a.insecure);
        assert!(!a.no_color);
    }

    #[test]
    fn args_parses_interval_short_and_long() {
        let a = try_parse(&["vozltop", "http://x/s", "-i", "0.5"]).unwrap();
        assert_eq!(a.interval, 0.5);
        let a = try_parse(&["vozltop", "http://x/s", "--interval", "2.5"]).unwrap();
        assert_eq!(a.interval, 2.5);
    }

    #[test]
    fn args_rejects_out_of_range_interval() {
        let err = try_parse(&["vozltop", "http://x/s", "-i", "0.01"]).unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::ValueValidation);
        let err = try_parse(&["vozltop", "http://x/s", "-i", "61"]).unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::ValueValidation);
    }

    #[test]
    fn args_parses_user_into_tuple() {
        let a = try_parse(&["vozltop", "http://x/s", "-u", "alice:s3cret"]).unwrap();
        assert_eq!(a.user.as_ref().unwrap().0, "alice");
        assert_eq!(a.user.as_ref().unwrap().1, "s3cret");
    }

    #[test]
    fn args_rejects_invalid_user_at_clap_layer() {
        let err = try_parse(&["vozltop", "http://x/s", "-u", "no_colon"]).unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::ValueValidation);
    }

    #[test]
    fn args_parses_repeated_headers() {
        let a = try_parse(&[
            "vozltop",
            "http://x/s",
            "-H",
            "Authorization: Bearer xyz",
            "-H",
            "X-Trace-Id: abc-123",
        ])
        .unwrap();
        assert_eq!(a.headers.len(), 2);
        assert_eq!(a.headers[0].0.as_str(), "authorization");
        assert_eq!(a.headers[0].1.to_str().unwrap(), "Bearer xyz");
        assert_eq!(a.headers[1].0.as_str(), "x-trace-id");
    }

    #[test]
    fn args_rejects_invalid_header_at_clap_layer() {
        let err = try_parse(&["vozltop", "http://x/s", "-H", "no-colon-here"]).unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::ValueValidation);
    }

    #[test]
    fn args_insecure_flag() {
        let a = try_parse(&["vozltop", "http://x/s", "--insecure"]).unwrap();
        assert!(a.insecure);
    }

    #[test]
    fn args_no_color_flag() {
        let a = try_parse(&["vozltop", "http://x/s", "--no-color"]).unwrap();
        assert!(a.no_color);
    }

    #[test]
    fn args_rejects_invalid_url() {
        // url::Url::from_str はスキーマ無しを弾く
        let err = try_parse(&["vozltop", "not a url"]).unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::ValueValidation);
    }

    // ---------- help / version ----------

    #[test]
    fn help_lists_all_flags() {
        let mut cmd = Args::command();
        let help = cmd.render_help().to_string();
        // 全フラグの説明が出ること (CLAUDE.md の CLI セクション準拠)
        for token in [
            "--interval",
            "--user",
            "--header",
            "--insecure",
            "--no-color",
        ] {
            assert!(help.contains(token), "help should mention {token}:\n{help}");
        }
    }

    #[test]
    fn version_flag_works() {
        let err = try_parse(&["vozltop", "--version"]).unwrap_err();
        // --version は clap が `DisplayVersion` で正常終了させる
        assert_eq!(err.kind(), clap::error::ErrorKind::DisplayVersion);
    }

    // ---------- no_color_effective ----------

    fn args_with_no_color(flag: bool) -> Args {
        Args {
            url: Url::parse("http://x/s").unwrap(),
            interval: 1.0,
            user: None,
            headers: Vec::new(),
            insecure: false,
            no_color: flag,
        }
    }

    #[test]
    fn no_color_effective_true_when_flag_set() {
        let a = args_with_no_color(true);
        // env が空のテスト隔離のため一時的に unset
        let _guard = ScopedEnv::remove("NO_COLOR");
        assert!(a.no_color_effective());
    }

    #[test]
    fn no_color_effective_false_when_flag_and_env_both_unset() {
        let a = args_with_no_color(false);
        let _guard = ScopedEnv::remove("NO_COLOR");
        assert!(!a.no_color_effective());
    }

    #[test]
    fn no_color_effective_true_when_env_set_non_empty() {
        let a = args_with_no_color(false);
        let _guard = ScopedEnv::set("NO_COLOR", "1");
        assert!(a.no_color_effective());
    }

    #[test]
    fn no_color_effective_false_when_env_empty() {
        // https://no-color.org: "present and not an empty string"
        let a = args_with_no_color(false);
        let _guard = ScopedEnv::set("NO_COLOR", "");
        assert!(!a.no_color_effective());
    }

    /// テスト中に環境変数を一時的に変更/復元するガード。
    ///
    /// `set` / `remove` で Drop 時に元の値に戻す。スレッド安全ではないので
    /// NO_COLOR 系テストは `cargo test` が default のスレッド並列実行下で
    /// 競合する可能性がある。ただし全テストが同じ env を読むだけ (= guard で
    /// 一貫性を保つだけ) のため、ここでは許容する。
    struct ScopedEnv {
        key: &'static str,
        original: Option<std::ffi::OsString>,
    }
    impl ScopedEnv {
        fn set(key: &'static str, value: &str) -> Self {
            let original = env::var_os(key);
            env::set_var(key, value);
            Self { key, original }
        }
        fn remove(key: &'static str) -> Self {
            let original = env::var_os(key);
            env::remove_var(key);
            Self { key, original }
        }
    }
    impl Drop for ScopedEnv {
        fn drop(&mut self) {
            match &self.original {
                Some(v) => env::set_var(self.key, v),
                None => env::remove_var(self.key),
            }
        }
    }
}
