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
use std::ffi::OsString;
use std::fs;
use std::io::{self, BufRead};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use clap::Parser;
use reqwest::header::{HeaderName, HeaderValue};
use url::Url;

use crate::config::{Config, ConfigError, HostConfig};

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
    // 引数完全に無し (= `vozltop` のみ) で起動された場合は、`<URL>...` が
    // 足りないという素っ気ないエラーではなく `--help` を出して exit 0 する。
    // URL を伴わない他フラグだけ (例: `vozltop --insecure`) のときは従来通り
    // MissingRequiredArgument エラーを返す (= 意図して何かを指定したのに URL
    // を忘れたケースは「忘れもの」として報告する方が親切)。
    arg_required_else_help = true,
)]
pub struct Args {
    /// nginx-vts の `/status/format/json` などを指す絶対 URL。
    ///
    /// issue #44: 1 つ以上の URL を受け取る。複数指定すると multi-host モードで
    /// 起動し、UI 上段に Host タブバーが出る。1 つだけのときは単一 host (= 従来通り)。
    /// `@alias` 引数 (config 経由) も同じ positional に書ける。`@alias` 単独のときは
    /// host config の各種フラグも引き継ぐ。複数 `@alias` のときはフラグ継承は行わず
    /// URL の置換のみ行う (per-host CLI フラグは v1 範囲外、issue #44 escalation #4 参照)。
    #[arg(value_name = "URL", num_args = 1.., required = true)]
    pub urls: Vec<Url>,

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

    /// 行の p95 レイテンシがこの ms 以上ならアラート表示 (ハイライト + ベル)。
    #[arg(long = "alert-p95-ms", value_name = "MS")]
    pub alert_p95_ms: Option<u64>,

    /// TOML 設定ファイルのパス (issue #46)。
    ///
    /// 未指定の場合は `$VOZLTOP_CONFIG` 環境変数、それも無ければ
    /// XDG ベース (`~/.config/vozltop/config.toml` 等) を探索する。
    /// 存在しない場合は config 無しで動作。`@alias` 引数で利用する。
    #[arg(long = "config", value_name = "PATH")]
    pub config: Option<PathBuf>,
}

/// `Args::parse_with_config` 等で失敗したときの統合エラー型。
#[derive(Debug)]
pub enum ConfigArgsError {
    /// config 読込 / alias 解決のエラー。
    Config(ConfigError),
    /// clap パースのエラー (= 不正引数)。`clap::Error` をそのまま包む。
    Clap(clap::Error),
}

impl std::fmt::Display for ConfigArgsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigArgsError::Config(e) => write!(f, "{e}"),
            ConfigArgsError::Clap(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ConfigArgsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ConfigArgsError::Config(e) => Some(e),
            ConfigArgsError::Clap(e) => Some(e),
        }
    }
}

impl From<ConfigError> for ConfigArgsError {
    fn from(e: ConfigError) -> Self {
        ConfigArgsError::Config(e)
    }
}

impl From<clap::Error> for ConfigArgsError {
    fn from(e: clap::Error) -> Self {
        ConfigArgsError::Clap(e)
    }
}

impl Args {
    /// 通常のエントリポイント。argv + config を読み、`@alias` を解決した最終 Args を返す。
    ///
    /// alias 経由 (`vozltop @prod`) の場合、`[hosts.prod]` の URL に positional 引数を
    /// 置換し、CLI で未指定だった `--user` / `--header` / `--interval` / `--insecure` /
    /// `--no-color` / `--alert-p95-ms` を host config の値で補完する。
    /// CLI で明示されたフラグは config を上書きする (CLI > config[hosts.<alias>])。
    /// `[defaults]` は本 PR ではスコープ外 (alias 経由でも使わない)。
    pub fn parse_with_config() -> Result<Self, ConfigArgsError> {
        Self::parse_with_config_from(env::args_os())
    }

    /// `parse_with_config` のテスト用版 (argv を引数化)。
    pub fn parse_with_config_from<I, S>(argv: I) -> Result<Self, ConfigArgsError>
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString> + Clone,
    {
        let mut argv: Vec<OsString> = argv.into_iter().map(Into::into).collect();
        // Pass 1: --config フラグを peek (clap parse 前に config path を確定する)。
        let explicit_path = find_config_flag_value(&argv);
        // Pass 2: positional の @alias を探す (issue #44: 複数 alias 対応)。
        let alias_positions = find_positional_alias_indices(&argv);
        if !alias_positions.is_empty() {
            // alias が見つかったときだけ config を読む。alias 無しなら従来動作。
            let config = Config::load(explicit_path.as_deref())?;
            // URL の置換は全 alias に対して行う。host config 由来のフラグ継承は
            // alias が 1 つだけのときのみ適用 (multi-alias で互いに矛盾する
            // フラグを注入できないため。詳細は urls の docstring 参照)。
            let single_alias = alias_positions.len() == 1;
            // 後ろから処理することで、フラグ append による index ずれを避けつつ
            // 「最後の URL 置換」が argv に与える影響を限定できる。
            for (idx, alias) in alias_positions.into_iter().rev() {
                let host = config.resolve_alias(&alias)?;
                if single_alias {
                    apply_host_to_argv(&mut argv, idx, host);
                } else {
                    argv[idx] = OsString::from(&host.url);
                }
            }
        }
        match Self::try_parse_from(argv) {
            Ok(args) => Ok(args),
            Err(e) => {
                // clap は `--version` / `--help` を Err(clap::Error) で返す
                // (kind = DisplayVersion / DisplayHelp)。これらは「正常な情報表示」
                // 用の特殊な variant で、本来 stdout に書いて exit 0 すべき。
                // 通常の Args::parse() なら clap が内部で e.exit() を呼ぶが、
                // ここは try_parse_from 経由なので呼び出し側で同等の処理が必要。
                // それ以外の parse error は従来通り ConfigArgsError::Clap で返し、
                // main.rs 側の color-eyre フォーマッタに任せる。
                use clap::error::ErrorKind;
                match e.kind() {
                    // `--help` / `--version` は clap が exit 0 + stdout で扱う。
                    ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => e.exit(),
                    // `arg_required_else_help` で help を出すケース。clap は
                    // この kind を「失敗扱い」とみなし `Error::print()` は stderr
                    // に出して `exit()` は 2 を返すが、本ツールでは引数なし起動の
                    // help 表示は `--help` と同じ「正常な情報表示」として扱いたい。
                    // `Display` impl は help テキストそのものを返すので stdout に
                    // 直接書いて exit 0 で抜ける。
                    ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => {
                        print!("{e}");
                        std::process::exit(0);
                    }
                    _ => {}
                }
                Err(e.into())
            }
        }
    }

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

/// argv に秘匿情報が露出していそうか判定する (issue #40 / #107)。
///
/// プロセスの argv をなめて以下のいずれかに該当する場合 `true`:
///
/// - `--user <user>:<pass>` の `pass` が非空かつ `-` 以外 (stdin 経由ではない)
///   かつ `VOZLTOP_PASSWORD` 環境変数が **未設定**
/// - `--header` 引数が `@` 始まりではなく、value 部分が `Bearer ` / `Basic ` で始まる
///   (= Authorization 系のトークンが平文で argv に乗っている可能性大)
/// - positional な URL 引数の userinfo に **非空 password** が含まれる
///   (例: `vozltop https://admin:secret@host/...`)。
///   `--user` と違って `VOZLTOP_PASSWORD` でも上書きされないため、env がセットされていても警告する。
///   username のみ (`https://admin@host/...`) は curl 互換で警告対象外。
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
    // 直前の引数が「値を取るフラグ」だった場合 true。次の引数は positional として
    // 扱わず URL 検査の対象外にする (例: `--user alice:pw` の `alice:pw` を URL として
    // パースしようとしない)。`=` 一体型 (`--user=alice:pw`) はこの状態には入らない。
    let mut prev_takes_value = false;
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
            if !v.starts_with(HEADER_FILE_PREFIX) {
                if let Some((_, value)) = v.split_once(':') {
                    let value = value.trim_start_matches([' ', '\t']);
                    if value.starts_with("Bearer ") || value.starts_with("Basic ") {
                        return true;
                    }
                }
            }
        }

        // issue #107: positional な URL に userinfo password が含まれているか。
        // - idx 0 (program name) はスキップ
        // - 直前のフラグが「値を取る」だった場合、その値はフラグの引数なのでスキップ
        // - フラグ自身 (`-` 始まり) もスキップし、値を取るフラグなら次反復用に flag を立てる
        // - それ以外 (= 純粋な positional) は URL として parse を試み、password が
        //   非空なら警告対象
        let take_value_was_pending = prev_takes_value;
        prev_takes_value = false;
        if idx == 0 || take_value_was_pending {
            continue;
        }
        if arg.starts_with('-') {
            // bare 形式 (`--user X`) のみ次の arg を値として消費。`=` 一体型は単独で完結。
            prev_takes_value = matches!(
                arg.as_str(),
                "-u" | "--user" | "-H" | "--header" | "-i" | "--interval" | "--alert-p95-ms"
            );
            continue;
        }
        if let Ok(url) = Url::parse(arg) {
            if url.password().is_some_and(|p| !p.is_empty()) {
                return true;
            }
        }
    }
    false
}

/// argv から `--config <path>` / `--config=<path>` を探して `PathBuf` を返す。
///
/// clap parse 前の peek 用 (config を読んで alias 解決するため)。複数指定された場合は
/// 最後の値を採用 (clap の "last wins" と整合)。
fn find_config_flag_value(argv: &[OsString]) -> Option<PathBuf> {
    let mut last: Option<PathBuf> = None;
    let mut iter = argv.iter().enumerate();
    while let Some((_, arg)) = iter.next() {
        let s = arg.to_string_lossy();
        if let Some(v) = s.strip_prefix("--config=") {
            last = Some(PathBuf::from(v));
        } else if s == "--config" {
            if let Some((_, next)) = iter.next() {
                last = Some(PathBuf::from(next));
            }
        }
    }
    last
}

/// argv の全 positional 引数を走査し、`@alias` 形式のものを `(index, alias)` の
/// Vec で返す (issue #44: 複数 alias 対応)。
///
/// 値を取るフラグ (`--user`, `-u`, `--header`, `-H`, `--interval`, `-i`,
/// `--alert-p95-ms`, `--config`) の直後の引数は positional ではなく
/// 値とみなす。`detect_argv_secret_in` と同じスキップ規則。
///
/// program name (`argv[0]`) もスキップ。通常の URL positional は結果に含めない
/// (alias のみ収集する)。
fn find_positional_alias_indices(argv: &[OsString]) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut prev_takes_value = false;
    for (idx, arg) in argv.iter().enumerate() {
        let s = arg.to_string_lossy();
        if idx == 0 || prev_takes_value {
            prev_takes_value = false;
            continue;
        }
        if s.starts_with('-') {
            prev_takes_value = matches!(
                s.as_ref(),
                "-u" | "--user"
                    | "-H"
                    | "--header"
                    | "-i"
                    | "--interval"
                    | "--alert-p95-ms"
                    | "--config"
            );
            continue;
        }
        if let Some(alias) = Config::parse_alias_arg(&s) {
            out.push((idx, alias.to_string()));
        }
        // 通常 URL positional は alias でないので結果に入れないが、
        // 走査は止めずに残りも続ける (issue #44: 複数 positional 対応)。
    }
    out
}

/// host config の値を argv に注入する。CLI で明示済みのフラグは上書きしない。
///
/// - `argv[alias_idx]` を `host.url` に書き換え
/// - `host.user`/`headers`/`interval`/`insecure`/`no_color`/`alert_p95_ms`
///   のうち、CLI に同名フラグが無いものを末尾に append する (clap が後勝ちなので
///   prepend より append の方が「CLI が後に来て上書きする」の semantics と整合)
pub(crate) fn apply_host_to_argv(argv: &mut Vec<OsString>, alias_idx: usize, host: &HostConfig) {
    // URL 置換
    argv[alias_idx] = OsString::from(&host.url);

    // CLI に存在するフラグ名のセット (bare 形式 / = 一体型 / 短縮形)。
    let cli_flags: Vec<String> = argv
        .iter()
        .map(|s| s.to_string_lossy().into_owned())
        .collect();
    let has_flag = |bare: &[&str]| {
        cli_flags.iter().any(|s| {
            bare.iter()
                .any(|b| s == b || s.starts_with(&format!("{b}=")))
        })
    };

    if let Some(user) = &host.user {
        if !has_flag(&["--user", "-u"]) {
            argv.push(OsString::from("--user"));
            argv.push(OsString::from(user));
        }
    }
    if let Some(headers) = &host.headers {
        if !has_flag(&["--header", "-H"]) {
            for h in headers {
                argv.push(OsString::from("--header"));
                argv.push(OsString::from(h));
            }
        }
    }
    if let Some(interval) = host.interval {
        if !has_flag(&["--interval", "-i"]) {
            argv.push(OsString::from("--interval"));
            argv.push(OsString::from(interval.to_string()));
        }
    }
    if host.insecure == Some(true) && !has_flag(&["--insecure"]) {
        argv.push(OsString::from("--insecure"));
    }
    if host.no_color == Some(true) && !has_flag(&["--no-color"]) {
        argv.push(OsString::from("--no-color"));
    }
    if let Some(ms) = host.alert_p95_ms {
        if !has_flag(&["--alert-p95-ms"]) {
            argv.push(OsString::from("--alert-p95-ms"));
            argv.push(OsString::from(ms.to_string()));
        }
    }
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
    fn args_with_no_args_shows_help() {
        // 完全な引数なし起動は `arg_required_else_help` 経由で help 表示。
        // clap 上は `DisplayHelpOnMissingArgumentOrSubcommand` Err として返る
        // (`--help` フラグでの help と区別される)。main 側でこの kind を見て
        // stdout 出力 + exit 0 に上書きしている。
        let err = try_parse(&["vozltop"]).unwrap_err();
        assert_eq!(
            err.kind(),
            clap::error::ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
        );
    }

    #[test]
    fn args_with_only_flags_still_requires_url() {
        // URL 以外のフラグだけ指定された場合 (= 何か入力する気はあったが URL を
        // 忘れたケース) は従来どおり MissingRequiredArgument エラー。
        let err = try_parse(&["vozltop", "--insecure"]).unwrap_err();
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

    // ---------- issue #47: alert 閾値フラグ ----------

    #[test]
    fn args_alert_flags_default_to_none() {
        let a = try_parse(&["vozltop", "http://x/s"]).unwrap();
        assert_eq!(a.alert_p95_ms, None);
    }

    #[test]
    fn args_parses_alert_flags() {
        let a = try_parse(&["vozltop", "http://x/s", "--alert-p95-ms", "500"]).unwrap();
        assert_eq!(a.alert_p95_ms, Some(500));
    }

    #[test]
    fn args_rejects_non_integer_alert_p95_ms_at_clap_layer() {
        // u64 パーサが小数 / 非数値を弾く (負値は clap が先頭 `-` を別フラグ扱いするため別系統)。
        let err = try_parse(&["vozltop", "http://x/s", "--alert-p95-ms", "1.5"]).unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::ValueValidation);
        let err = try_parse(&["vozltop", "http://x/s", "--alert-p95-ms", "abc"]).unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::ValueValidation);
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
            urls: vec![Url::parse("http://x/s").unwrap()],
            interval: 1.0,
            user: None,
            headers: Vec::new(),
            insecure: false,
            no_color: flag,
            alert_p95_ms: None,
            config: None,
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

    // ---------- issue #107: URL userinfo の credentials 検知 ----------

    #[test]
    fn detect_argv_url_with_password_returns_true() {
        let argv = vec![
            "vozltop".to_string(),
            "https://admin:secret@nginx.example.com/status/format/json".to_string(),
        ];
        assert!(detect_argv_secret_in(&argv, None));
    }

    #[test]
    fn detect_argv_url_with_password_returns_true_even_when_env_set() {
        // issue #107: URL に埋め込んだ credentials は VOZLTOP_PASSWORD では上書きされない
        // (env は `--user` の password にしか効かない)。argv に残るため env がセット
        // されていても警告する。
        let argv = vec![
            "vozltop".to_string(),
            "https://admin:secret@host/status".to_string(),
        ];
        assert!(detect_argv_secret_in(
            &argv,
            Some("env_pass_does_not_save_url".to_string())
        ));
    }

    #[test]
    fn detect_argv_url_username_only_returns_false() {
        // username のみ (password 無し) は curl 互換で警告対象外。
        let argv = vec![
            "vozltop".to_string(),
            "https://admin@host/status".to_string(),
        ];
        assert!(!detect_argv_secret_in(&argv, None));
    }

    #[test]
    fn detect_argv_url_empty_password_returns_false() {
        // `user:` (空 password) は警告対象外 (--user 側の挙動と整合)。
        let argv = vec![
            "vozltop".to_string(),
            "https://admin:@host/status".to_string(),
        ];
        assert!(!detect_argv_secret_in(&argv, None));
    }

    #[test]
    fn detect_argv_url_without_userinfo_returns_false() {
        // 通常 URL (credentials 無し) は警告対象外。回帰防止用に明示。
        let argv = vec![
            "vozltop".to_string(),
            "https://nginx.example.com/status/format/json".to_string(),
        ];
        assert!(!detect_argv_secret_in(&argv, None));
    }

    #[test]
    fn detect_argv_url_with_flags_after_returns_true() {
        // URL の後にフラグが続くケース。位置に関わらず検出する。
        let argv = vec![
            "vozltop".to_string(),
            "https://admin:secret@host/status".to_string(),
            "--interval".to_string(),
            "0.5".to_string(),
        ];
        assert!(detect_argv_secret_in(&argv, None));
    }

    #[test]
    fn detect_argv_url_with_flags_before_returns_true() {
        // フラグが URL より先に来るケース。bare 形式 `-i 0.5` の値 `0.5` を URL として
        // 誤って parse しないこと (`0.5` は Url::parse で error)。
        let argv = vec![
            "vozltop".to_string(),
            "--interval".to_string(),
            "0.5".to_string(),
            "https://admin:secret@host/status".to_string(),
        ];
        assert!(detect_argv_secret_in(&argv, None));
    }

    #[test]
    fn detect_argv_user_value_is_not_parsed_as_url() {
        // `--user alice:s3cret` の `alice:s3cret` が誤って URL として parse され
        // 警告するのを防ぐ (既存の --user 経路で env_set=true のとき) — というよりも
        // URL parse ロジックがフラグの値を踏まないことの回帰防止。
        let argv = vec![
            "vozltop".to_string(),
            "https://host/status".to_string(),
            "--user".to_string(),
            "alice:s3cret".to_string(),
        ];
        // env がセット済みなので --user 側は false。URL 側も userinfo 無しなので false。
        // 結果: false (= alice:s3cret が URL として参照されない証拠)。
        assert!(!detect_argv_secret_in(&argv, Some("env_pass".to_string())));
    }

    #[test]
    fn detect_argv_url_with_eq_form_flag_returns_true() {
        // `--user=...` 一体型でも URL 検査は正しく動く。
        let argv = vec![
            "vozltop".to_string(),
            "--user=alice:plain".to_string(), // 既存ロジックで true
            "https://host/status".to_string(),
        ];
        // alice:plain で既に true になるが、URL の検査経路が阻害されないことの確認
        assert!(detect_argv_secret_in(&argv, None));
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

    // ---------- issue #46: parse_with_config (config 経由の alias 解決) ----------

    fn write_tmp_config(content: &str) -> std::path::PathBuf {
        let dir = env::temp_dir().join(format!(
            "vozltop-config-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn parse_with_config_without_alias_is_identity() {
        // alias を使わない場合は config を読まずに従来動作 (URL 直指定が通る)。
        let argv = ["vozltop", "https://example.com/status"];
        let args = Args::parse_with_config_from(argv).unwrap();
        assert_eq!(args.urls[0].as_str(), "https://example.com/status");
        assert_eq!(args.interval, 1.0); // 組み込みデフォルト
    }

    #[test]
    fn parse_with_config_resolves_alias_to_url() {
        // alias の URL を CLI 引数 position に substitute する。
        let cfg = write_tmp_config(
            "[hosts.prod]\nurl = \"https://nginx.example.com/status/format/json\"\n",
        );
        let argv = ["vozltop", "@prod", "--config", cfg.to_str().unwrap()];
        let args = Args::parse_with_config_from(argv).unwrap();
        assert_eq!(
            args.urls[0].as_str(),
            "https://nginx.example.com/status/format/json"
        );
    }

    #[test]
    fn parse_with_config_applies_host_config_when_cli_not_specified() {
        // host config の interval / user を CLI 未指定時に補完。
        let cfg = write_tmp_config(
            "[hosts.prod]\nurl = \"https://h/s\"\nuser = \"alice:s3cret\"\ninterval = 0.5\n",
        );
        let argv = ["vozltop", "@prod", "--config", cfg.to_str().unwrap()];
        let args = Args::parse_with_config_from(argv).unwrap();
        assert_eq!(args.interval, 0.5);
        assert_eq!(
            args.user.as_ref().map(|(u, p)| (u.as_str(), p.as_str())),
            Some(("alice", "s3cret"))
        );
    }

    #[test]
    fn parse_with_config_cli_overrides_host_config() {
        // CLI で明示した interval が host config を上書きする。
        let cfg = write_tmp_config("[hosts.prod]\nurl = \"https://h/s\"\ninterval = 0.5\n");
        let argv = [
            "vozltop",
            "@prod",
            "--config",
            cfg.to_str().unwrap(),
            "--interval",
            "2.0",
        ];
        let args = Args::parse_with_config_from(argv).unwrap();
        assert_eq!(args.interval, 2.0, "CLI が host config を上書き");
    }

    #[test]
    fn parse_with_config_alias_with_unknown_name_errors() {
        let cfg = write_tmp_config("[hosts.prod]\nurl = \"https://h/s\"\n");
        let argv = ["vozltop", "@missing", "--config", cfg.to_str().unwrap()];
        let err = Args::parse_with_config_from(argv).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("missing"), "msg: {msg}");
        assert!(msg.contains("[hosts.*]"), "msg: {msg}");
    }

    #[test]
    fn parse_with_config_alias_without_config_file_errors() {
        // 明示 --config が指す path が無い場合はエラー。
        let argv = [
            "vozltop",
            "@prod",
            "--config",
            "/nonexistent/path/__nope__.toml",
        ];
        let err = Args::parse_with_config_from(argv).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("config file") || msg.contains("nonexistent"),
            "msg: {msg}"
        );
    }

    #[test]
    fn find_config_flag_value_handles_eq_form() {
        let argv: Vec<OsString> = ["vozltop", "--config=/a/b.toml", "@prod"]
            .iter()
            .map(|s| OsString::from(*s))
            .collect();
        assert_eq!(
            find_config_flag_value(&argv),
            Some(PathBuf::from("/a/b.toml"))
        );
    }

    #[test]
    fn find_config_flag_value_handles_bare_form() {
        let argv: Vec<OsString> = ["vozltop", "--config", "/a/b.toml", "@prod"]
            .iter()
            .map(|s| OsString::from(*s))
            .collect();
        assert_eq!(
            find_config_flag_value(&argv),
            Some(PathBuf::from("/a/b.toml"))
        );
    }

    #[test]
    fn find_config_flag_value_none_when_absent() {
        let argv: Vec<OsString> = ["vozltop", "https://x"]
            .iter()
            .map(|s| OsString::from(*s))
            .collect();
        assert!(find_config_flag_value(&argv).is_none());
    }

    #[test]
    fn find_positional_alias_indices_skips_flag_values() {
        // `--user alice:pw` の値 `alice:pw` を positional として誤検出しないこと。
        let argv: Vec<OsString> = ["vozltop", "--user", "alice:pw", "@prod"]
            .iter()
            .map(|s| OsString::from(*s))
            .collect();
        assert_eq!(
            find_positional_alias_indices(&argv),
            vec![(3, "prod".to_string())]
        );
    }

    // ---------- issue #44: 複数 URL / 複数 alias ----------

    #[test]
    fn args_parses_multiple_urls() {
        let a = try_parse(&[
            "vozltop",
            "http://host1/status",
            "http://host2/status",
            "http://host3/status",
        ])
        .unwrap();
        assert_eq!(a.urls.len(), 3);
        assert_eq!(a.urls[0].as_str(), "http://host1/status");
        assert_eq!(a.urls[2].as_str(), "http://host3/status");
    }

    #[test]
    fn args_single_url_is_a_one_element_vec() {
        // 単一 host の backward 互換性: urls の長さは 1。
        let a = try_parse(&["vozltop", "http://host/status"]).unwrap();
        assert_eq!(a.urls.len(), 1);
    }

    #[test]
    fn find_positional_alias_indices_collects_all_aliases() {
        let argv: Vec<OsString> = ["vozltop", "@prod", "@staging", "@edge"]
            .iter()
            .map(|s| OsString::from(*s))
            .collect();
        let got = find_positional_alias_indices(&argv);
        assert_eq!(got.len(), 3);
        assert_eq!(got[0], (1, "prod".to_string()));
        assert_eq!(got[1], (2, "staging".to_string()));
        assert_eq!(got[2], (3, "edge".to_string()));
    }

    #[test]
    fn find_positional_alias_indices_mixes_urls_and_aliases() {
        let argv: Vec<OsString> = ["vozltop", "http://x/s", "@staging"]
            .iter()
            .map(|s| OsString::from(*s))
            .collect();
        let got = find_positional_alias_indices(&argv);
        assert_eq!(got, vec![(2, "staging".to_string())]);
    }

    #[test]
    fn parse_with_config_resolves_multiple_aliases() {
        let cfg = write_tmp_config(
            "[hosts.prod]\nurl = \"https://prod/s\"\n\n[hosts.stg]\nurl = \"https://stg/s\"\n",
        );
        let argv = [
            "vozltop",
            "@prod",
            "@stg",
            "--config",
            cfg.to_str().unwrap(),
        ];
        let args = Args::parse_with_config_from(argv).unwrap();
        assert_eq!(args.urls.len(), 2);
        assert_eq!(args.urls[0].as_str(), "https://prod/s");
        assert_eq!(args.urls[1].as_str(), "https://stg/s");
    }

    #[test]
    fn parse_with_config_multi_alias_does_not_inject_per_host_interval() {
        // 複数 alias のとき、host config の interval は適用しない (グローバルな
        // CLI フラグは単一値しか持てないため。詳細は urls の docstring 参照)。
        let cfg = write_tmp_config(
            "[hosts.prod]\nurl = \"https://prod/s\"\ninterval = 0.5\n\n[hosts.stg]\nurl = \"https://stg/s\"\ninterval = 2.0\n",
        );
        let argv = [
            "vozltop",
            "@prod",
            "@stg",
            "--config",
            cfg.to_str().unwrap(),
        ];
        let args = Args::parse_with_config_from(argv).unwrap();
        // 組み込みデフォルト (1.0) のまま (どちらの alias の interval も適用されない)
        assert_eq!(args.interval, 1.0);
    }
}
