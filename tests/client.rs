//! `VtsClient::fetch` の最小限の動作確認。
//!
//! issue #36 で `httpmock` 等の本格的なモックを使った網羅的な E2E テストを
//! 追加する予定。本 PR では「正常 fetch」「2xx 以外」「接続失敗」の 3 系を
//! tokio の `TcpListener` で手書きのワンショットサーバを立てて検証する。

use std::time::Duration;

use reqwest::header::{HeaderName, HeaderValue};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use url::Url;
use vozltop::cli::Args;
use vozltop::client::{FetchError, VtsClient};

/// ワンショット HTTP サーバを立てて URL を返す。
///
/// `tokio::spawn` 内で `accept()` → リクエストを 1 件受けたら `response`
/// バイト列をそのまま流して `close` する。HTTP/1.1 の最低限の構文 (status
/// line + `Connection: close` + 空行 + body) だけ満たしている。
async fn spawn_oneshot_server(response: Vec<u8>) -> Url {
    let (url, _rx) = spawn_oneshot_capturing(response).await;
    url
}

/// ワンショット HTTP サーバを立てて `(URL, 受信したリクエストの byte 列)` を返す。
///
/// `rx` を `await` するとサーバが受け取ったリクエスト (request line + headers + 必要なら body)
/// が 1 回ぶん String で返る。auth ヘッダ等が実際に送られているかを検証する用途。
async fn spawn_oneshot_capturing(response: Vec<u8>) -> (Url, oneshot::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    let (tx, rx) = oneshot::channel();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept");
        let mut buf = vec![0u8; 8192];
        let n = socket.read(&mut buf).await.expect("read");
        let request = String::from_utf8_lossy(&buf[..n]).into_owned();
        let _ = tx.send(request);
        socket.write_all(&response).await.expect("write");
        socket.shutdown().await.ok();
    });
    let url = format!("http://{addr}/status/format/json").parse().unwrap();
    (url, rx)
}

fn args_for(url: Url) -> Args {
    Args {
        url,
        interval: 1.0,
        user: None,
        headers: Vec::new(),
        insecure: false,
        no_color: false,
    }
}

/// テスト用の `--header` 値 (raw 文字列) をパース済みタプルに変換する。
/// `cli::parse_header` と等価な処理を最小限で再現するヘルパ。
fn header(name: &'static str, value: &'static str) -> (HeaderName, HeaderValue) {
    (
        HeaderName::from_static(name),
        HeaderValue::from_static(value),
    )
}

/// 最小の有効 VtsStatus JSON + 200 OK レスポンスの HTTP/1.1 byte 列。
fn ok_vts_response() -> Vec<u8> {
    let body = r#"{
        "hostName": "h", "nginxVersion": "1", "moduleVersion": "v",
        "loadMsec": 0, "nowMsec": 1,
        "connections": {"active":0,"reading":0,"writing":0,"waiting":0,"accepted":0,"handled":0,"requests":0}
    }"#;
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    )
    .into_bytes()
}

#[tokio::test]
async fn fetch_decodes_200_response() {
    // 最小限の VtsStatus を構成: serverZones / upstreamZones / cacheZones は
    // optional default なので、必須フィールドだけ埋めれば decode できる
    let body = r#"{
        "hostName": "test-host",
        "nginxVersion": "1.27.3",
        "moduleVersion": "v0.2.5",
        "loadMsec": 1000000,
        "nowMsec": 1000500,
        "connections": {
            "active": 1, "reading": 0, "writing": 1, "waiting": 0,
            "accepted": 42, "handled": 42, "requests": 42
        }
    }"#;
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let url = spawn_oneshot_server(response.into_bytes()).await;
    let client = VtsClient::new(&args_for(url)).expect("client builds");

    let status = client.fetch().await.expect("fetch succeeds");
    assert_eq!(status.host_name, "test-host");
    assert_eq!(status.nginx_version, "1.27.3");
    assert_eq!(status.connections.requests, 42);
    assert!(status.server_zones.is_empty());
}

#[tokio::test]
async fn fetch_decodes_when_content_type_is_not_json() {
    // VTS module は `Content-Type: text/plain` で返すバージョンもある。
    // body が JSON ならデコードできるべき。
    let body = r#"{
        "hostName": "h", "nginxVersion": "1", "moduleVersion": "v",
        "loadMsec": 0, "nowMsec": 1,
        "connections": {"active":0,"reading":0,"writing":0,"waiting":0,"accepted":0,"handled":0,"requests":0}
    }"#;
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let url = spawn_oneshot_server(response.into_bytes()).await;
    let client = VtsClient::new(&args_for(url)).expect("client builds");
    let status = client.fetch().await.expect("decode ignores Content-Type");
    assert_eq!(status.host_name, "h");
}

#[tokio::test]
async fn fetch_returns_err_on_non_2xx() {
    let body = "Not Found";
    let response = format!(
        "HTTP/1.1 404 Not Found\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let url = spawn_oneshot_server(response.into_bytes()).await;
    let client = VtsClient::new(&args_for(url)).expect("client builds");

    let err = match client.fetch().await {
        Ok(_) => panic!("404 should be Err"),
        Err(e) => e,
    };
    assert!(
        matches!(err, FetchError::Status { code } if code.as_u16() == 404),
        "expected FetchError::Status(404), got {err:?}"
    );
    // banner_message は URL/secret を含まないが status code は含む
    assert!(err.banner_message().contains("404"));
}

#[tokio::test]
async fn fetch_returns_err_on_invalid_json() {
    let body = "<html>this is not JSON</html>";
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let url = spawn_oneshot_server(response.into_bytes()).await;
    let client = VtsClient::new(&args_for(url)).expect("client builds");

    let err = match client.fetch().await {
        Ok(_) => panic!("non-JSON body should be Err"),
        Err(e) => e,
    };
    assert!(
        matches!(err, FetchError::Decode(_)),
        "expected FetchError::Decode, got {err:?}"
    );
    assert_eq!(err.banner_message(), "invalid VTS JSON");
}

#[tokio::test]
async fn fetch_returns_err_on_connection_refused() {
    // ポートを bind してすぐに drop して closed なポートを得る。
    // 直後の connect は ECONNREFUSED または connect timeout で必ず失敗する。
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    let url: Url = format!("http://{addr}/status/format/json").parse().unwrap();
    let client = VtsClient::new(&args_for(url)).expect("client builds");

    // 5s connect_timeout より十分短いタイムアウトで wrap し、テストが
    // ハングしないことを保証する
    let fetched = tokio::time::timeout(Duration::from_secs(6), client.fetch()).await;
    let res = fetched.expect("test should not exceed 6s");
    let err = match res {
        Ok(_) => panic!("connect to a closed port should return Err"),
        Err(e) => e,
    };
    // ECONNREFUSED は Connect、connect_timeout 経由なら Timeout のいずれか
    assert!(
        matches!(err, FetchError::Connect(_) | FetchError::Timeout),
        "expected Connect or Timeout, got {err:?}"
    );
}

// ---- issue #18: authentication / TLS flags --------------------------------

#[tokio::test]
async fn fetch_sends_basic_auth_when_user_set() {
    let (url, rx) = spawn_oneshot_capturing(ok_vts_response()).await;
    let args = Args {
        url,
        interval: 1.0,
        user: Some(("alice".into(), "s3cret".into())),
        headers: Vec::new(),
        insecure: false,
        no_color: false,
    };
    let client = VtsClient::new(&args).expect("client builds");
    client.fetch().await.expect("fetch succeeds");

    let request = rx.await.expect("request captured");
    // "Basic " + base64("alice:s3cret") = "Basic YWxpY2U6czNjcmV0"
    assert!(
        request.contains("authorization: Basic YWxpY2U6czNjcmV0")
            || request.contains("Authorization: Basic YWxpY2U6czNjcmV0"),
        "expected Authorization header in request:\n{request}"
    );
}

#[tokio::test]
async fn fetch_sends_custom_headers() {
    let (url, rx) = spawn_oneshot_capturing(ok_vts_response()).await;
    let args = Args {
        url,
        interval: 1.0,
        user: None,
        headers: vec![
            header("authorization", "Bearer xyz"),
            header("x-trace-id", "abc-123"),
        ],
        insecure: false,
        no_color: false,
    };
    let client = VtsClient::new(&args).expect("client builds");
    client.fetch().await.expect("fetch succeeds");

    let request = rx.await.expect("request captured");
    // header 名は HTTP/1.1 では case-insensitive、reqwest は小文字化して送る
    assert!(
        request.to_lowercase().contains("authorization: bearer xyz"),
        "expected Authorization: Bearer xyz in:\n{request}"
    );
    assert!(
        request.to_lowercase().contains("x-trace-id: abc-123"),
        "expected X-Trace-Id: abc-123 in:\n{request}"
    );
}

#[tokio::test]
async fn fetch_user_takes_precedence_over_authorization_header() {
    // --user で指定した basic_auth は per-request で適用される。
    // 一方 --header 'Authorization: Bearer ...' は default_headers として積まれる。
    // reqwest の挙動として per-request basic_auth は default Authorization を
    // 上書きするので、最終的に送られるのは Basic ... になる。
    let (url, rx) = spawn_oneshot_capturing(ok_vts_response()).await;
    let args = Args {
        url,
        interval: 1.0,
        user: Some(("alice".into(), "s3cret".into())),
        headers: vec![header("authorization", "Bearer should-be-overridden")],
        insecure: false,
        no_color: false,
    };
    let client = VtsClient::new(&args).expect("client builds");
    client.fetch().await.expect("fetch succeeds");

    let request = rx.await.expect("request captured");
    let lower = request.to_lowercase();
    assert!(
        lower.contains("authorization: basic"),
        "Basic auth should win over default header in:\n{request}"
    );
    assert!(
        !lower.contains("authorization: bearer"),
        "Bearer header should be overridden by basic_auth in:\n{request}"
    );
}

#[tokio::test]
async fn client_builds_with_insecure_flag() {
    // --insecure を立てた状態で reqwest::ClientBuilder が成立し、平文 HTTP
    // (証明書検証関係なし) でも普通に fetch できることを確認。
    // TLS 自己署名の動作検証はクレートテストでは難しいため、構築 + 通信成立を
    // 確認するに留める。
    let (url, _rx) = spawn_oneshot_capturing(ok_vts_response()).await;
    let args = Args {
        url,
        interval: 1.0,
        user: None,
        headers: Vec::new(),
        insecure: true,
        no_color: true, // 警告のカラーコードを抑制 (テスト出力を汚さない)
    };
    let client = VtsClient::new(&args).expect("insecure client builds");
    client
        .fetch()
        .await
        .expect("insecure client still fetches plain HTTP");
}

// 旧版の `invalid_user_format_is_rejected_at_client_new` / `invalid_header_format_is_rejected_at_client_new`
// は issue #34 で clap の `value_parser` 側に検証を移したため削除した。
// 同等のケースは `src/cli.rs` の `args_rejects_invalid_user_at_clap_layer` /
// `args_rejects_invalid_header_at_clap_layer` で確認している。
