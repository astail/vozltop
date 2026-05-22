//! `VtsClient::fetch` の最小限の動作確認。
//!
//! issue #36 で `httpmock` 等の本格的なモックを使った網羅的な E2E テストを
//! 追加する予定。本 PR では「正常 fetch」「2xx 以外」「接続失敗」の 3 系を
//! tokio の `TcpListener` で手書きのワンショットサーバを立てて検証する。

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use url::Url;
use vozltop::cli::Args;
use vozltop::client::VtsClient;

/// ワンショット HTTP サーバを立てて URL を返す。
///
/// `tokio::spawn` 内で `accept()` → リクエストを 1 件受けたら `response`
/// バイト列をそのまま流して `close` する。HTTP/1.1 の最低限の構文 (status
/// line + `Connection: close` + 空行 + body) だけ満たしている。
async fn spawn_oneshot_server(response: Vec<u8>) -> Url {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept");
        let mut buf = [0u8; 4096];
        let _ = socket.read(&mut buf).await;
        socket.write_all(&response).await.expect("write");
        socket.shutdown().await.ok();
    });
    format!("http://{addr}/status/format/json").parse().unwrap()
}

fn args_for(url: Url) -> Args {
    Args { url }
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

    let err = client.fetch().await.expect_err("404 should be Err");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("404"),
        "error message should include 404: {msg}"
    );
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

    let err = client
        .fetch()
        .await
        .expect_err("non-JSON body should be Err");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("decode VTS JSON"),
        "error message should mention JSON decode failure: {msg}"
    );
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
    assert!(
        res.is_err(),
        "connect to a closed port should return Err, got {res:?}"
    );
}
