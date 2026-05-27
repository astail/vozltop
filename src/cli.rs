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
use std::fs;
use std::io::{self, BufRead};
use std::path::Path;
use std::str::FromStr;

use clap::Parser;
use reqwest::header::{HeaderName, HeaderValue};
use url::Url;

/// `VOZLTOP_PASSWORD` 環境変数: 設定時は `--user user:...` の password を
/// argv ではなく環境から取得 (issue #40)。共有マシンで `ps` から password が
/// 漏れるのを防ぐ。値が空文字列の場合は「設定無し」扱い。
pub const PASSWORD_ENV_VAR: &str = "VOZLTOP_PASSWORD";

/// `--user user:-` の password 値: stdin 1 行から読み取る合図 (issue #40)。
pub const STDIN_PASSWORD_TOKEN: &str = "-";

/// `--header @path/to/file` の prefix (issue #40)。ファイルから `K: V` 行を 1 つ読む。
pub const HEADER_FILE_PREFIX: char = '@';

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

    /// `--user` の最終 password を解決する (issue #40)。優先度:
    ///
    /// 1. `VOZLTOP_PASSWORD` 環境変数 (非空) — argv の password を **上書き**
    /// 2. argv の password が `-` リテラル — stdin 1 行を読む
    ///    (TTY からの読み取りは echo 抑制を実装していないため拒否)
    /// 3. argv の password をそのまま使用
    ///
    /// `--user` 自体が指定されていなければ `Ok(None)`。
    ///
    /// stdin / env を直接見ない純粋関数版は [`resolve_user_with`]。
    pub fn resolved_user(&self) -> Result<Option<(String, String)>, String> {
        resolve_user_with(self.user.as_ref(), env::var(PASSWORD_ENV_VAR).ok(), || {
            read_password_from_stdin()
        })
    }
}

/// `Args::resolved_user` の純粋関数版 (テスト用に env / stdin を引数化)。
///
/// - `argv_user`: `--user` パース結果
/// - `env_password`: `VOZLTOP_PASSWORD` 環境変数値 (未設定なら `None`、空文字列も `None` 扱い)
/// - `read_stdin`: stdin 1 行読み込み関数 (`user:-` のとき呼ばれる)
pub fn resolve_user_with(
    argv_user: Option<&(String, String)>,
    env_password: Option<String>,
    read_stdin: impl FnOnce() -> Result<String, String>,
) -> Result<Option<(String, String)>, String> {
    let Some((user, argv_pass)) = argv_user else {
        return Ok(None);
    };

    // 1. env が非空ならそれを採用 (argv を上書き)
    if let Some(env_pass) = env_password.filter(|v| !v.is_empty()) {
        return Ok(Some((user.clone(), env_pass)));
    }

    // 2. argv の password が `-` なら stdin
    if argv_pass == STDIN_PASSWORD_TOKEN {
        let pass = read_stdin()?;
        return Ok(Some((user.clone(), pass)));
    }

    // 3. argv をそのまま
    Ok(Some((user.clone(), argv_pass.clone())))
}

/// stdin の 1 行を password として読み取る。
///
/// TTY (= `stdin().is_terminal()`) からは拒否する: echo 抑制を実装しておらず
/// 対話入力で password が画面に残るリスクがあるため。pipe / redirect 経由なら OK。
fn read_password_from_stdin() -> Result<String, String> {
    use std::io::IsTerminal;
    let stdin = io::stdin();
    if stdin.is_terminal() {
        return Err(
            "--user user:- requires stdin to be a pipe/redirect, not a TTY. \
             Use VOZLTOP_PASSWORD env var for interactive use."
                .to_string(),
        );
    }
    let mut buf = String::new();
    stdin
        .lock()
        .read_line(&mut buf)
        .map_err(|e| format!("--user user:-: failed to read stdin: {e}"))?;
    // 末尾の改行を除去 (CRLF / LF 両対応)
    let pass = buf.trim_end_matches(['\r', '\n']).to_string();
    Ok(pass)
}

/// `NO_COLOR` 環境変数の判定 (https://no-color.org/)。
pub fn no_color_env() -> bool {
    matches!(env::var_os("NO_COLOR"), Some(v) if !v.is_empty())
}

/// argv に秘匿情報が露出していそうか判定する (issue #40)。
///
/// プロセスの argv をなめて以下のいずれかに該当する場合 `true`:
///
/// - `--user <user>:<pass>` の `pass` が非空かつ `-` 以外 (stdin 経由ではない)
///   かつ `VOZLTOP_PASSWORD` 環境変数が **未設定**
/// - `--header` 引数が `@` 始まりではなく、value 部分が `Bearer ` / `Basic ` で始まる
///   (= Authorization 系のトークンが平文で argv に乗っている可能性大)
///
/// `OsString` を一度全部 `String` 化するため非 UTF-8 引数は判定対象外
/// (実害無し: 非 UTF-8 ならいずれ clap で reject される)。
///
/// テスト時は [`detect_argv_secret_in`] に直接 args の slice を渡す。
pub fn detect_argv_secret() -> bool {
    let argv: Vec<String> = env::args().collect();
    detect_argv_secret_in(&argv, env::var(PASSWORD_ENV_VAR).ok())
}

/// [`detect_argv_secret`] の純粋関数版 (テスト用)。
pub fn detect_argv_secret_in(argv: &[String], env_password: Option<String>) -> bool {
    let env_set = env_password.is_some_and(|v| !v.is_empty());
    for (idx, arg) in argv.iter().enumerate() {
        // --user X:Y / -u X:Y
        let user_value = if arg == "--user" || arg == "-u" {
            argv.get(idx + 1).map(String::as_str)
        } else {
            arg.strip_prefix("--user=")
                .or_else(|| arg.strip_prefix("-u="))
        };
        if let Some(v) = user_value {
            if let Some((_, pass)) = v.split_once(':') {
                if !pass.is_empty() && pass != STDIN_PASSWORD_TOKEN && !env_set {
                    return true;
                }
            }
        }

        // --header X / -H X (header value が Authorization 系か)
        let header_value = if arg == "--header" || arg == "-H" {
            argv.get(idx + 1).map(String::as_str)
        } else {
            arg.strip_prefix("--header=")
                .or_else(|| arg.strip_prefix("-H="))
        };
        if let Some(v) = header_value {
            if v.starts_with(HEADER_FILE_PREFIX) {
                continue;
            }
            if let Some((_, value)) = v.split_once(':') {
                let value = value.trim_start_matches([' ', '\t']);
                if value.starts_with("Bearer ") || value.starts_with("Basic ") {
                    return true;
                }
            }
        }
    }
    false
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
///
/// **`@path/to/file` 構文** (issue #40): 引数全体が `@` で始まる場合は
/// ファイルパスとして開き、最初の非空・非コメント (`#` 始まり) 行を `K: V` として
/// パースする。argv に secrets を残さないためのフォールバック。
pub fn parse_header(s: &str) -> Result<(HeaderName, HeaderValue), String> {
    // @file 構文: 中身を読み出して再帰的に parse_header_inline へ
    if let Some(path) = s.strip_prefix(HEADER_FILE_PREFIX) {
        return parse_header_from_file(Path::new(path));
    }
    parse_header_inline(s)
}

/// `--header @path` のファイル展開 (issue #40)。
///
/// - 最初の非空・非コメント行を `K: V` としてパース
/// - 末尾改行 / CR は除去
/// - ファイル open 失敗 / 読み取り失敗はエラー
/// - 行が見つからない場合もエラー
fn parse_header_from_file(path: &Path) -> Result<(HeaderName, HeaderValue), String> {
    let content = fs::read_to_string(path)
        .map_err(|e| format!("--header @{}: cannot read file: {e}", path.display()))?;
    let line = content
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'))
        .ok_or_else(|| {
            format!(
                "--header @{}: file contains no non-empty, non-comment line",
                path.display()
            )
        })?;
    parse_header_inline(line).map_err(|e| format!("--header @{}: {e}", path.display()))
}

/// `--header` の純粋なインライン版 (`@file` を解さない)。
fn parse_header_inline(s: &str) -> Result<(HeaderName, HeaderValue), String> {
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
        let _env = crate::test_util::ENV_LOCK.lock().unwrap();
        let a = args_with_no_color(true);
        // env が空のテスト隔離のため一時的に unset
        let _guard = ScopedEnv::remove("NO_COLOR");
        assert!(a.no_color_effective());
    }

    #[test]
    fn no_color_effective_false_when_flag_and_env_both_unset() {
        let _env = crate::test_util::ENV_LOCK.lock().unwrap();
        let a = args_with_no_color(false);
        let _guard = ScopedEnv::remove("NO_COLOR");
        assert!(!a.no_color_effective());
    }

    #[test]
    fn no_color_effective_true_when_env_set_non_empty() {
        let _env = crate::test_util::ENV_LOCK.lock().unwrap();
        let a = args_with_no_color(false);
        let _guard = ScopedEnv::set("NO_COLOR", "1");
        assert!(a.no_color_effective());
    }

    #[test]
    fn no_color_effective_false_when_env_empty() {
        // https://no-color.org: "present and not an empty string"
        let _env = crate::test_util::ENV_LOCK.lock().unwrap();
        let a = args_with_no_color(false);
        let _guard = ScopedEnv::set("NO_COLOR", "");
        assert!(!a.no_color_effective());
    }

    // ---------- issue #40: resolve_user_with ----------

    fn ok_stdin_reader(value: &'static str) -> impl FnOnce() -> Result<String, String> {
        move || Ok(value.to_string())
    }

    #[test]
    fn resolve_user_returns_none_when_no_argv_user() {
        let got = resolve_user_with(None, None, ok_stdin_reader("never")).unwrap();
        assert_eq!(got, None);
    }

    #[test]
    fn resolve_user_passes_through_argv_when_no_env_no_stdin() {
        let user = ("alice".to_string(), "secret".to_string());
        let got = resolve_user_with(Some(&user), None, ok_stdin_reader("never")).unwrap();
        assert_eq!(got, Some(("alice".to_string(), "secret".to_string())));
    }

    #[test]
    fn resolve_user_env_overrides_argv() {
        let user = ("alice".to_string(), "argv_pass".to_string());
        let got = resolve_user_with(
            Some(&user),
            Some("env_pass".to_string()),
            ok_stdin_reader("never"),
        )
        .unwrap();
        assert_eq!(got, Some(("alice".to_string(), "env_pass".to_string())));
    }

    #[test]
    fn resolve_user_env_empty_string_falls_back_to_argv() {
        // VOZLTOP_PASSWORD="" は「未設定」扱い (NO_COLOR と同等のセマンティクス)
        let user = ("alice".to_string(), "argv_pass".to_string());
        let got =
            resolve_user_with(Some(&user), Some(String::new()), ok_stdin_reader("never")).unwrap();
        assert_eq!(got, Some(("alice".to_string(), "argv_pass".to_string())));
    }

    #[test]
    fn resolve_user_stdin_token_reads_stdin() {
        let user = ("alice".to_string(), STDIN_PASSWORD_TOKEN.to_string());
        let got = resolve_user_with(Some(&user), None, ok_stdin_reader("from_stdin")).unwrap();
        assert_eq!(got, Some(("alice".to_string(), "from_stdin".to_string())));
    }

    #[test]
    fn resolve_user_env_takes_precedence_over_stdin_token() {
        // env が設定されていれば user:- でも stdin は読まない (副作用を防ぐ)
        let user = ("alice".to_string(), STDIN_PASSWORD_TOKEN.to_string());
        let got = resolve_user_with(Some(&user), Some("env_pass".to_string()), || {
            panic!("stdin reader must not be called")
        })
        .unwrap();
        assert_eq!(got, Some(("alice".to_string(), "env_pass".to_string())));
    }

    #[test]
    fn resolve_user_stdin_error_propagates() {
        let user = ("alice".to_string(), STDIN_PASSWORD_TOKEN.to_string());
        let got = resolve_user_with(Some(&user), None, || Err("stdin is TTY".to_string()));
        assert!(got.is_err());
    }

    // ---------- issue #40: parse_header @file ----------

    fn write_tmp_file(content: &str) -> std::path::PathBuf {
        let dir = env::temp_dir().join(format!(
            "vozltop-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("header.txt");
        fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn parse_header_at_file_reads_first_line() {
        let path = write_tmp_file("Authorization: Bearer xyz\n");
        let arg = format!("@{}", path.display());
        let (name, value) = parse_header(&arg).unwrap();
        assert_eq!(name.as_str(), "authorization");
        assert_eq!(value.to_str().unwrap(), "Bearer xyz");
    }

    #[test]
    fn parse_header_at_file_skips_blank_and_comment_lines() {
        let path = write_tmp_file("# this is a comment\n\n   \nAuthorization: Bearer xyz\n");
        let arg = format!("@{}", path.display());
        let (name, value) = parse_header(&arg).unwrap();
        assert_eq!(name.as_str(), "authorization");
        assert_eq!(value.to_str().unwrap(), "Bearer xyz");
    }

    #[test]
    fn parse_header_at_file_missing_path_errors() {
        let err = parse_header("@/nonexistent/path/__not_there__").unwrap_err();
        assert!(err.starts_with("--header @"));
        assert!(err.contains("cannot read file"));
    }

    #[test]
    fn parse_header_at_file_only_comments_errors() {
        let path = write_tmp_file("# only comments\n# nothing else\n");
        let arg = format!("@{}", path.display());
        let err = parse_header(&arg).unwrap_err();
        assert!(err.contains("no non-empty, non-comment line"));
    }

    #[test]
    fn parse_header_inline_unchanged_by_at_file_support() {
        // 既存挙動が壊れていないこと
        let (name, value) = parse_header("X-Trace-Id: 12345").unwrap();
        assert_eq!(name.as_str(), "x-trace-id");
        assert_eq!(value.to_str().unwrap(), "12345");
    }

    // ---------- issue #40: detect_argv_secret_in ----------

    #[test]
    fn detect_argv_no_flags_returns_false() {
        let argv = vec!["vozltop".to_string(), "http://x/s".to_string()];
        assert!(!detect_argv_secret_in(&argv, None));
    }

    #[test]
    fn detect_argv_user_with_password_returns_true() {
        let argv = vec![
            "vozltop".to_string(),
            "http://x/s".to_string(),
            "--user".to_string(),
            "alice:secret".to_string(),
        ];
        assert!(detect_argv_secret_in(&argv, None));
    }

    #[test]
    fn detect_argv_user_with_empty_password_returns_false() {
        // alice: (空 password) は curl 同様に対話プロンプト/将来拡張用なので警告対象外
        let argv = vec![
            "vozltop".to_string(),
            "--user".to_string(),
            "alice:".to_string(),
        ];
        assert!(!detect_argv_secret_in(&argv, None));
    }

    #[test]
    fn detect_argv_user_with_stdin_token_returns_false() {
        let argv = vec![
            "vozltop".to_string(),
            "--user".to_string(),
            "alice:-".to_string(),
        ];
        assert!(!detect_argv_secret_in(&argv, None));
    }

    #[test]
    fn detect_argv_user_with_env_set_returns_false() {
        // env で上書きされる前提なので argv に書かれていても警告しない
        let argv = vec![
            "vozltop".to_string(),
            "--user".to_string(),
            "alice:secret".to_string(),
        ];
        assert!(!detect_argv_secret_in(&argv, Some("env_pass".to_string())));
    }

    #[test]
    fn detect_argv_user_eq_form_detected() {
        let argv = vec!["vozltop".to_string(), "--user=alice:secret".to_string()];
        assert!(detect_argv_secret_in(&argv, None));
    }

    #[test]
    fn detect_argv_header_bearer_returns_true() {
        let argv = vec![
            "vozltop".to_string(),
            "--header".to_string(),
            "Authorization: Bearer xyz".to_string(),
        ];
        assert!(detect_argv_secret_in(&argv, None));
    }

    #[test]
    fn detect_argv_header_basic_returns_true() {
        let argv = vec![
            "vozltop".to_string(),
            "-H".to_string(),
            "Authorization: Basic YWxpY2U=".to_string(),
        ];
        assert!(detect_argv_secret_in(&argv, None));
    }

    #[test]
    fn detect_argv_header_at_file_returns_false() {
        let argv = vec![
            "vozltop".to_string(),
            "--header".to_string(),
            "@/etc/vozltop/auth-header".to_string(),
        ];
        assert!(!detect_argv_secret_in(&argv, None));
    }

    #[test]
    fn detect_argv_header_non_auth_returns_false() {
        let argv = vec![
            "vozltop".to_string(),
            "--header".to_string(),
            "X-Trace-Id: 12345".to_string(),
        ];
        assert!(!detect_argv_secret_in(&argv, None));
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
