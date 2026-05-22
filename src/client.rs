//! VTS `/status/format/json` を 1 回 fetch する HTTP クライアント。
//!
//! issue #17 では認証なしの最小実装。issue #18 で `--user` / `--header` /
//! `--insecure` を `Args` から拾うように拡張する想定。

use std::time::Duration;

use color_eyre::eyre::{eyre, Result, WrapErr};
use reqwest::Client;
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
pub struct VtsClient {
    url: Url,
    http: Client,
}

impl VtsClient {
    /// `Args` から HTTP クライアントを組み立てる。
    ///
    /// reqwest の `ClientBuilder` 構築は I/O を伴わないが、`build()` は失敗
    /// しうる (システム証明書のロードなど) ため `Result` で返す。
    pub fn new(args: &Args) -> Result<Self> {
        let http = Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .wrap_err("failed to build reqwest::Client")?;

        Ok(Self {
            url: args.url.clone(),
            http,
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
        let response = self
            .http
            .get(self.url.clone())
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
