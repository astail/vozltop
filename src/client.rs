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

/// 1 レスポンスとして受理する body の上限 (バイト)。
///
/// 10 MiB。脅威モデル (issue #41):
///
/// - ユーザが誤って vts 以外の巨大 endpoint (バックアップダンプ等) を指した
/// - 内部サーバが侵害され、巨大レスポンスで `vozltop` を OOM させようとした
///
/// 観測値の参考: histogram bucket 付き serverZones + upstreamZones を多数持つ
/// 構成でも実 JSON は数百 KB に収まる。10 MiB はその ×20〜×100 の安全側マージン。
///
/// `Content-Length` が宣言されていればそこで即拒否し、無い (chunked) 場合は
/// ストリーム読み取り中に累積バイト数で打ち切る。
pub const MAX_RESPONSE_BYTES: usize = 10 * 1024 * 1024;

/// `read_body_with_limit` の初期 capacity 上限 (1 MiB)。
///
/// `Content-Length` が攻撃者の意図で過大に詰められていた場合、それを
/// `Vec::with_capacity` にそのまま渡すと事前確保で OOM し得る。
/// プレチェックを抜けた値 (`MAX_RESPONSE_BYTES` 以下) でも、最初は 1 MiB
/// 程度に抑えて、必要に応じて Vec が自動拡張する。
const INITIAL_BODY_CAPACITY: usize = 1024 * 1024;

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

        // issue #40: VOZLTOP_PASSWORD 環境変数 / --user user:- (stdin) を解決。
        // reqwest 構築の問題を先に出すため build() の後に解決する。
        let basic_auth = args
            .resolved_user()
            .map_err(|e| color_eyre::eyre::eyre!("{e}"))?;

        // build() が成功した後で初めて警告する。
        // build() 失敗時に「警告だけ見せて死ぬ」混乱を避けるため。
        if args.insecure {
            warn_insecure(args.no_color_effective());
        }
        if crate::cli::detect_argv_secret() {
            warn_argv_secrets(args.no_color_effective());
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

        // issue #41: 巨大レスポンスによる OOM/DoS を防ぐ。
        // (1) Content-Length を信用するプレチェック (1 byte も読まずに reject)
        // (2) chunked encoding 等で Content-Length が無い場合は、stream を
        //     `chunk()` で受けながら累積で打ち切る。
        let body = read_body_with_limit(response, MAX_RESPONSE_BYTES).await?;

        serde_json::from_slice::<VtsStatus>(&body).map_err(FetchError::Decode)
    }
}

/// レスポンス body を `max_bytes` を上限としてメモリに読み込む。
///
/// - `Content-Length: N` が宣言され、`N > max_bytes` なら 1 byte も読まずに reject
/// - そうでなければ `Response::chunk()` をループで読み、累積が `max_bytes` を超えた
///   時点で reject (chunked encoding / 巨大ストリーム対策)
///
/// `Vec` の初期 capacity は (a) Content-Length が分かっていればそれ
/// (b) 不明なら [`INITIAL_BODY_CAPACITY`] を上限として確保する。`Content-Length`
/// が `max_bytes` 直前まで詰められていた場合に capacity だけで OOM するのを防ぐ。
async fn read_body_with_limit(
    mut response: reqwest::Response,
    max_bytes: usize,
) -> std::result::Result<Vec<u8>, FetchError> {
    // Content-Length をローカルに束縛して以後 3 経路で使い回す:
    // (1) プレチェック (2) 初期 capacity 計算 (3) 上限超過時の advertised 報告。
    // chunked encoding では None のまま。
    let advertised = response.content_length();

    // (1) Content-Length プレチェック
    if let Some(n) = advertised {
        if n > max_bytes as u64 {
            return Err(FetchError::ResponseTooLarge {
                limit: max_bytes,
                advertised: Some(n),
            });
        }
    }

    // (2) chunked stream を累積で打ち切る
    let initial = advertised
        .map(|n| (n as usize).min(INITIAL_BODY_CAPACITY))
        .unwrap_or(8 * 1024);
    let mut body: Vec<u8> = Vec::with_capacity(initial);

    while let Some(chunk) = response.chunk().await? {
        // 受け取った時点で上限超過なら body にコピーせず即拒否する。
        // chunk 自体は既に reqwest が tokio buffer に積んでいるが、
        // それを Vec に展開する前に止めれば追加 alloc を防げる。
        if body.len().saturating_add(chunk.len()) > max_bytes {
            return Err(FetchError::ResponseTooLarge {
                limit: max_bytes,
                advertised,
            });
        }
        body.extend_from_slice(&chunk);
    }

    Ok(body)
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
    /// レスポンス body が [`MAX_RESPONSE_BYTES`] を超えた (issue #41)。
    ///
    /// `advertised` は `Content-Length` で宣言された値 (信じられる場合のみ
    /// `Some`)。chunked encoding 経由で実ストリームが上限を超えた場合は
    /// `None` になる。
    ResponseTooLarge {
        limit: usize,
        advertised: Option<u64>,
    },
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
            FetchError::ResponseTooLarge { limit, .. } => {
                // 単位を MiB (1024×1024) で人間可読化。limit は静的に決まるので
                // 端数を考えず割り算する (`MAX_RESPONSE_BYTES` は 10 MiB)。
                let mib = limit / (1024 * 1024);
                format!("response too large (limit {mib} MiB)")
            }
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
            FetchError::Timeout
            | FetchError::Status { .. }
            | FetchError::ResponseTooLarge { .. } => None,
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
///
/// TUI 上の常時 banner 表示 (`Connection insecure`) は header UI 実装時
/// (issue #27) に `App` 側で `args.insecure` を参照して描画する。issue #39 の
/// TUI banner 要件はその時点で完了とする。
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

/// argv に password / Bearer トークンが平文で乗っているときの起動時警告 (issue #40)。
///
/// 共有ホストでは `ps` 出力に argv が見えるため、`--user user:pass` や
/// `--header 'Authorization: Bearer ...'` を直接渡すと他ユーザに漏れる。
/// 回避策 (env / @file / stdin) を 1 行で示す。
///
/// stderr 出力 + ANSI 黄色 (太字)。`OnceLock` で 1 度きり。
fn warn_argv_secrets(no_color: bool) {
    static WARNED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    WARNED.get_or_init(|| {
        let msg = "WARNING: secrets in argv are visible to other users via `ps`. \
                   Consider using VOZLTOP_PASSWORD env, --user user:- (stdin), \
                   or --header @file to avoid exposing them.";
        if no_color {
            eprintln!("{msg}");
        } else {
            eprintln!("\x1b[1;33m{msg}\x1b[0m");
        }
    });
}
