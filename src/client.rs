//! VTS `/status/format/json` を 1 回 fetch する HTTP クライアント。
//!
//! issue #17 で認証なし最小実装、issue #18 で `--user` / `--header` /
//! `--insecure` を `Args` から拾うように拡張、issue #19 で `fetch` の戻り値
//! 型を `Result<VtsStatus, FetchError>` に分類した。issue #34 で
//! `Args` 側を clap derive に置き換えた結果、ヘッダと user:pass は
//! **既に正規化済みの型** (`HeaderName` / `HeaderValue` / `(String, String)`)
//! として渡ってくる。本ファイルでは検証を再実行しない。

use std::fmt;
use std::time::Duration;

use color_eyre::eyre::{Result, WrapErr};
use reqwest::header::HeaderMap;
use reqwest::{Client, StatusCode};
use url::Url;

use crate::cli::Args;
use crate::model::VtsStatus;

/// `User-Agent` ヘッダの値 (例: `vozltop/0.1.0`)。
///
/// `concat!` で `&'static str` として組み立てる: 実行時生成のコストを避けつつ、
/// バージョン更新時に `Cargo.toml` 一箇所で済む。
pub const USER_AGENT: &str = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"));

/// 接続フェーズだけのタイムアウト。
/// 名前解決〜TCP/TLS ハンドシェイクを 5 秒で打ち切る。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// リクエスト全体のタイムアウト (接続 + 送信 + 受信)。
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// 1 回 fetch するごとに 1 つの `VtsStatus` を返す HTTP クライアント。
///
/// **セキュリティ運用ルール**: 本構造体には `#[derive(Debug)]` を絶対に追加
/// しない。`basic_auth` フィールドが password を `String` で平文保持しているため、
/// `Debug` が入ると `format!("{:?}")` 経由で password がログ・パニックメッセージ
/// 等に流出する。issue #40 (`VOZLTOP_PASSWORD` / argv 経由の平文露出問題) で
/// `secrecy::SecretString` 等の wrapper 型に置き換えるまで、この約束は厳守。
pub struct VtsClient {
    url: Url,
    http: Client,
    /// `--user` 指定時のみ `Some((user, pass))`。リクエストごとに `basic_auth`
    /// を適用する。`default_headers` に Authorization を入れない理由は、
    /// reqwest が `basic_auth` で `HeaderValue::set_sensitive(true)` を立てて
    /// くれて、ログ・デバッグ出力で値が伏字になるため。
    basic_auth: Option<(String, String)>,
}

impl VtsClient {
    /// `Args` から HTTP クライアントを組み立てる。
    ///
    /// reqwest の `ClientBuilder` 構築は I/O を伴わないが、`build()` は失敗
    /// しうる (システム証明書のロードなど) ため `Result` で返す。
    pub fn new(args: &Args) -> Result<Self> {
        let mut builder = Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT);

        // --header を default_headers に積む。
        // 同名ヘッダの繰り返しは HeaderMap::append で値を複数持たせる。
        if !args.headers.is_empty() {
            let mut headers = HeaderMap::new();
            for (name, value) in &args.headers {
                headers.append(name.clone(), value.clone());
            }
            builder = builder.default_headers(headers);
        }

        if args.insecure {
            builder = builder.danger_accept_invalid_certs(true);
        }

        let http = builder
            .build()
            .wrap_err("failed to build reqwest::Client")?;

        let basic_auth = args.user.clone();

        // build() が成功した後で初めて警告する。
        // build() 失敗時に「警告だけ見せて死ぬ」混乱を避けるため。
        if args.insecure {
            warn_insecure(args.no_color_effective());
        }

        Ok(Self {
            url: args.url.clone(),
            http,
            basic_auth,
        })
    }

    /// 1 回だけ GET して `VtsStatus` にデコードする。
    ///
    /// 戻り値は `Result<VtsStatus, FetchError>` で、エラーは
    /// `Connect` / `Timeout` / `Status` / `Decode` に分類される。
    /// 呼び出し側 (`state::App`) はこの分類と連続失敗カウンタで `Stale data` /
    /// `Disconnected` バナーを切り替える (issue #19)。
    ///
    /// 動作仕様:
    /// - 2xx 以外は `FetchError::Status { code }`
    /// - body は **常に** `serde_json::from_str` を試みる
    ///   (`Content-Type` を見ない理由は、`add_header Content-Type` を雑に
    ///   設定している nginx 構成でも fetch を成功させたいから)
    pub async fn fetch(&self) -> std::result::Result<VtsStatus, FetchError> {
        let mut req = self.http.get(self.url.clone());
        if let Some((user, pass)) = &self.basic_auth {
            req = req.basic_auth(user, Some(pass));
        }

        let response = req.send().await?;

        let status = response.status();
        if !status.is_success() {
            return Err(FetchError::Status { code: status });
        }

        // TODO(issue #41): レスポンスサイズ上限を導入する。現状 `response.text()`
        // は body 全体をメモリに読み込むため、悪意ある (or バグった) リバースプロキシ
        // が GB 級 body を返すと OOM する。
        let body = response.text().await?;

        serde_json::from_str::<VtsStatus>(&body).map_err(FetchError::Decode)
    }
}

/// fetch 1 回ぶんのエラー分類。
///
/// 連続失敗カウンタとバナー表示の切り替え判定に使う。`Display` 実装はバナー用
/// の **短い** メッセージ (URL や secret を含まない) を返す。
///
/// # セキュリティ運用ルール (issue #40 まで)
///
/// `FetchError::Connect(reqwest::Error)` の内部 `reqwest::Error` は `Debug`
/// 実装で URL を含む。ユーザが `vozltop http://admin:secret@host/...` のように
/// URL に basic auth を埋め込んで起動した場合、`format!("{err:?}")` 経由で
/// password がログに漏れる経路が成立する。
///
/// このため:
///
/// - **ユーザ向け表示は必ず `banner_message()` または `Display` (`{err}`) 経由で
///   行うこと**。`Debug` (`{err:?}`) はトラブルシュート時の手動操作に限定。
/// - 自動ログ・パニック message・スタックトレースに `FetchError` を `{:?}` で
///   流さない。
///
/// issue #40 (`VOZLTOP_PASSWORD` / argv 経由の平文露出問題) で URL credential
/// を sanitize する `Debug` 手書き実装に置き換えるまで、この約束は厳守。
#[derive(Debug)]
pub enum FetchError {
    /// 名前解決、TCP 接続、TLS ハンドシェイク等の接続フェーズの失敗。transient。
    Connect(reqwest::Error),
    /// `connect_timeout` または `timeout` の超過。transient。
    Timeout,
    /// 非 2xx レスポンス。4xx は permanent 寄り、5xx は transient 寄り。
    /// UI 上は両方とも失敗としてカウントする。
    Status { code: StatusCode },
    /// 200 が返ったが body を `VtsStatus` にデコードできなかった。
    /// スキーマ不一致 (permanent) も nginx 半起動 (transient) もありうる。
    Decode(serde_json::Error),
}

impl FetchError {
    /// バナー / ステータスバーに表示する短いメッセージ。
    ///
    /// URL や user:pass を絶対に含めない (1 行ログに secret が漏れないように)。
    pub fn banner_message(&self) -> String {
        match self {
            FetchError::Connect(_) => "connection failed".to_string(),
            FetchError::Timeout => "request timed out".to_string(),
            FetchError::Status { code } => match code.canonical_reason() {
                Some(reason) => format!("HTTP {} {reason}", code.as_u16()),
                None => format!("HTTP {}", code.as_u16()),
            },
            FetchError::Decode(_) => "invalid VTS JSON".to_string(),
        }
    }
}

impl fmt::Display for FetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.banner_message())
    }
}

impl std::error::Error for FetchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            FetchError::Connect(e) => Some(e),
            FetchError::Decode(e) => Some(e),
            FetchError::Timeout | FetchError::Status { .. } => None,
        }
    }
}

impl From<reqwest::Error> for FetchError {
    fn from(err: reqwest::Error) -> Self {
        // 分類順序は意図的: timeout は他フラグ (is_request など) と同時に
        // true になりうるので最優先で見る。
        if err.is_timeout() {
            FetchError::Timeout
        } else if err.is_status() {
            // 通常 fetch() 内の `error_for_status` を経由しないので
            // ここに到達することは稀。安全側でハンドリング。
            match err.status() {
                Some(code) => FetchError::Status { code },
                None => FetchError::Connect(err),
            }
        } else {
            // is_connect / is_request / is_body / is_decode (transport-level)
            // などはすべて Connect 扱いで吸収する。
            FetchError::Connect(err)
        }
    }
}

/// `--insecure` 時の起動時警告。
///
/// stderr 出力 + ANSI 黄色 (太字)。`no_color` が真の時は ANSI コードを
/// 出さない (`--no-color` または `NO_COLOR` 環境変数のいずれか;
/// 評価は `Args::no_color_effective` に集約)。
///
/// プロセス起動中に複数回 `VtsClient::new()` が呼ばれても (将来の reconnect
/// ロジック等)、警告が運用ログを汚さないよう `OnceLock` で 1 度きりに絞る。
fn warn_insecure(no_color: bool) {
    static WARNED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    WARNED.get_or_init(|| {
        let msg = "WARNING: --insecure disables TLS verification. Use only on trusted networks.";
        if no_color {
            eprintln!("{msg}");
        } else {
            // \x1b[1;33m = bold yellow, \x1b[0m = reset
            eprintln!("\x1b[1;33m{msg}\x1b[0m");
        }
    });
}
