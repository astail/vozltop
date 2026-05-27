//! `VtsClient` → `App` の状態遷移を、wiremock の HTTP mock サーバで end-to-end に
//! 検証する統合テスト (issue #36)。
//!
//! 目的:
//!
//! - 単体テスト (`src/**/tests`) では `App::on_fetch_ok` / `on_fetch_err` を直接呼んで
//!   状態機械を検証している。実際のネットワーク経路 (reqwest → tokio → fetch ループ)
//!   に対するリグレッションは別途必要。
//! - 本ファイルは「HTTP 越しに `VtsClient::fetch().await` を呼んで、戻り値を
//!   `App` に流す」流れを 1 本の test 関数に閉じ込め、各テストは <500ms
//!   (起動 + 数回の fetch) で完結する。
//!
//! ## 設計判断
//!
//! - **wiremock を選択** (httpmock ではなく):
//!   - tokio runtime で動く async API が中心。`#[tokio::test]` と相性が良い。
//!   - matcher (`method`, `path`, `basic_auth`) と `up_to_n_times` で「最初の 1 回
//!     だけ 200、以降 500」のような順序付き応答を簡潔に書ける。
//! - **localhost-only**: `MockServer::start().await` はランダムポートを bind する。
//!   外向き I/O 一切なし。CI で flaky になる経路は無い。
//! - **fixtures 共用**: `tests/fixtures/initial.json` を `include_str!` で埋め込み、
//!   tests/deserialize.rs と同じ実 nginx-vts 応答を使う。

use std::time::Duration;

use url::Url;
use vozltop::cli::Args;
use vozltop::client::VtsClient;
use vozltop::state::{App, AppStatus, DISCONNECTED_THRESHOLD};
use wiremock::matchers::{basic_auth, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// nginx-vts の `/status/format/json` の実応答 (`tests/fixtures/initial.json`)。
/// `cargo test` 中はビルド時に文字列に焼かれる。
const FIXTURE_BODY: &str = include_str!("fixtures/initial.json");

/// テスト用に最小構成の `Args` を組み立てる。
///
/// clap derive を経由したいところだが、`Args::parse_from` だと argv パースが
/// 走ってしまって `-u` 等のセマンティクスに引きずられる。フィールドが全て
/// `pub` で公開されている前提で直接構築する。
fn args_for(url: Url) -> Args {
    Args {
        url,
        interval: 1.0,
        user: None,
        headers: Vec::new(),
        insecure: false,
        no_color: true,
    }
}

/// `MockServer::uri()` + `/status/format/json` を URL に整形する。
fn fixture_url(server: &MockServer) -> Url {
    Url::parse(&format!("{}/status/format/json", server.uri()))
        .expect("MockServer::uri() returned an invalid URL")
}

// ---------- 1) Connecting → Running ----------

#[tokio::test]
async fn connecting_transitions_to_running_after_first_successful_fetch() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/status/format/json"))
        .respond_with(ResponseTemplate::new(200).set_body_string(FIXTURE_BODY))
        .expect(1)
        .mount(&server)
        .await;

    let args = args_for(fixture_url(&server));
    let client = VtsClient::new(&args).expect("VtsClient::new should succeed");
    let mut app = App::new();
    assert!(
        matches!(app.status, AppStatus::Connecting),
        "App は起動直後 Connecting"
    );

    let status = client
        .fetch()
        .await
        .expect("fixture を返す mock が 200 を返すはず");
    app.on_fetch_ok(status);

    assert!(
        matches!(app.status, AppStatus::Running),
        "1 回目の fetch 成功で Running へ遷移する"
    );
    assert_eq!(app.history.len(), 1, "history に snapshot が 1 件積まれる");
    assert!(app.error_banner.is_none(), "成功時は error_banner クリア");
    // expect(1) は MockServer Drop 時に違反検知 (panic) するため、ここでの追加 assert は不要。
}

// ---------- 2) Running → Stale → Disconnected (5 連続 500) ----------

#[tokio::test]
async fn five_consecutive_500_responses_escalate_running_to_disconnected() {
    let server = MockServer::start().await;

    // 最初の 1 回だけ 200 を返す mock。with_priority で 500 mock より優先する。
    Mock::given(method("GET"))
        .and(path("/status/format/json"))
        .respond_with(ResponseTemplate::new(200).set_body_string(FIXTURE_BODY))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;

    // 以降は 500 を無制限に返す fallback。
    Mock::given(method("GET"))
        .and(path("/status/format/json"))
        .respond_with(ResponseTemplate::new(500))
        .with_priority(5)
        .mount(&server)
        .await;

    let args = args_for(fixture_url(&server));
    let client = VtsClient::new(&args).expect("VtsClient::new should succeed");
    let mut app = App::new();

    // 1 回目: 200 → Running
    let status = client.fetch().await.expect("最初の fetch は 200");
    app.on_fetch_ok(status);
    assert!(matches!(app.status, AppStatus::Running));

    // 2〜THRESHOLD+1 回目: 500 を THRESHOLD 回 → Stale が積み上がって Disconnected に escalate
    for i in 0..DISCONNECTED_THRESHOLD {
        let err = client
            .fetch()
            .await
            .expect_err(&format!("{} 回目の 500 は err になる", i + 1));
        app.on_fetch_err(&err);
    }

    match app.status {
        AppStatus::Disconnected { failures } => {
            assert_eq!(
                failures, DISCONNECTED_THRESHOLD,
                "Stale から Disconnected への escalate 時点で failures = THRESHOLD"
            );
        }
        other => panic!("expected Disconnected, got {other:?}"),
    }
    assert!(
        app.error_banner.is_some(),
        "失敗中は error_banner が表示される"
    );
}

// ---------- 3) Basic 認証 (`--user`) が wire まで届く ----------

#[tokio::test]
async fn basic_auth_credentials_are_sent_with_fetch() {
    let server = MockServer::start().await;

    // `basic_auth` matcher は Authorization: Basic <base64(user:pass)> を厳密に
    // 比較する。マッチしなければ MockServer 既定動作で 404 が返り、状態遷移上は
    // 「成功しない」ことで間接的に「auth が届いていない」が検出できる。
    Mock::given(method("GET"))
        .and(path("/status/format/json"))
        .and(basic_auth("alice", "wonderland"))
        .respond_with(ResponseTemplate::new(200).set_body_string(FIXTURE_BODY))
        .expect(1)
        .mount(&server)
        .await;

    let mut args = args_for(fixture_url(&server));
    args.user = Some(("alice".to_string(), "wonderland".to_string()));
    let client = VtsClient::new(&args).expect("VtsClient::new should succeed");

    let status = tokio::time::timeout(Duration::from_millis(500), client.fetch())
        .await
        .expect("auth 付き fetch が 500ms 以内に返る")
        .expect("auth 一致で 200 が返る");

    let mut app = App::new();
    app.on_fetch_ok(status);
    assert!(matches!(app.status, AppStatus::Running));
    // expect(1) で mock 側からも厳密にカウント検証 (Drop 時に検査)。
}
