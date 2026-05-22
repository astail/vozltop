//! VTS `/status/format/json` を 1 回 fetch する HTTP クライアント。
//!
//! issue #17 で認証なし最小実装、issue #18 で `--user` / `--header` /
//! `--insecure` を `Args` から拾うように拡張。

use std::time::Duration;

use color_eyre::eyre::{eyre, Result, WrapErr};
use reqwest::header::HeaderMap;
use reqwest::Client;
use url::Url;

use crate::cli::{self, Args};
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
            for raw in &args.headers {
                let (name, value) = cli::parse_header(raw)?;
                headers.append(name, value);
            }
            builder = builder.default_headers(headers);
        }

        if args.insecure {
            builder = builder.danger_accept_invalid_certs(true);
        }

        let http = builder
            .build()
            .wrap_err("failed to build reqwest::Client")?;

        let basic_auth = match &args.user {
            Some(raw) => Some(cli::parse_user(raw)?),
            None => None,
        };

        // build() が成功し、認証情報の検証も通った後で初めて警告する。
        // build() 失敗時に「警告だけ見せて死ぬ」混乱を避けるため。
        if args.insecure {
            warn_insecure();
        }

        Ok(Self {
            url: args.url.clone(),
            http,
            basic_auth,
        })
    }

    /// 1 回だけ GET して `VtsStatus` にデコードする。
    ///
    /// 動作仕様:
    /// - 2xx 以外は `Err` (HTTP コードとフレーズを含む)
    /// - body は **常に** `serde_json::from_str` を試みる
    ///   (`Content-Type` を見ない理由は、`add_header Content-Type` を雑に
    ///   設定している nginx 構成でも fetch を成功させたいから)
    /// - JSON パース失敗時はデコード対象の先頭 200 バイトを Err メッセージに
    ///   含めて debug を助ける
    pub async fn fetch(&self) -> Result<VtsStatus> {
        let mut req = self.http.get(self.url.clone());
        if let Some((user, pass)) = &self.basic_auth {
            req = req.basic_auth(user, Some(pass));
        }

        let response = req
            .send()
            .await
            .wrap_err_with(|| format!("HTTP request to {} failed", self.url))?;

        let status = response.status();
        if !status.is_success() {
            return Err(eyre!(
                "HTTP {} {} from {}",
                status.as_u16(),
                status.canonical_reason().unwrap_or("(no reason phrase)"),
                self.url
            ));
        }

        // TODO(issue #41): レスポンスサイズ上限を導入する。現状 `response.text()`
        // は body 全体をメモリに読み込むため、悪意ある (or バグった) リバースプロキシ
        // が GB 級 body を返すと OOM する。
        let body = response
            .text()
            .await
            .wrap_err("failed to read response body")?;

        serde_json::from_str::<VtsStatus>(&body).wrap_err_with(|| {
            let preview: String = body.chars().take(200).collect();
            format!(
                "failed to decode VTS JSON from {} ({} bytes); first 200 chars: {:?}",
                self.url,
                body.len(),
                preview
            )
        })
    }
}

/// `--insecure` 時の起動時警告。
///
/// stderr 出力 + ANSI 黄色 (太字)。`NO_COLOR` 環境変数がセットされている時は
/// ANSI コードを出さない (<https://no-color.org/> 準拠)。
///
/// プロセス起動中に複数回 `VtsClient::new()` が呼ばれても (将来の reconnect
/// ロジック等)、警告が運用ログを汚さないよう `OnceLock` で 1 度きりに絞る。
fn warn_insecure() {
    static WARNED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    WARNED.get_or_init(|| {
        let msg = "WARNING: --insecure disables TLS verification. Use only on trusted networks.";
        if std::env::var_os("NO_COLOR").is_some() {
            eprintln!("{msg}");
        } else {
            // \x1b[1;33m = bold yellow, \x1b[0m = reset
            eprintln!("\x1b[1;33m{msg}\x1b[0m");
        }
    });
}
