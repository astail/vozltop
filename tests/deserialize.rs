//! `tests/fixtures/*.json` を `VtsStatus` にデコードできることを担保する。
//!
//! - `no_histogram.json`: `requestBuckets.msecs` / `.counters` が空配列のスキーマ
//! - `with_histogram.json`: `vhost_traffic_status_histogram_buckets` 設定時のスキーマ
//!
//! 値は issue #15 で取得した実フィクスチャから採取した整数で固定比較する。
//! これは「現コミットの fixture バイト列」と「現在の `VtsStatus` 型定義」が
//! 整合している強い保証になる一方、フィクスチャを `tests/fixtures/README.md`
//! の手順で再取得すると `host_name` (Docker container ID) や `connections.requests`
//! などが新しい値に変わるため、このファイル中の固定値も同時に更新する必要がある
//! ことに注意。再取得時の更新漏れを失敗 message で気付けるよう、敢えて exact-value
//! を残している (shape ベースの assert に倒すと「decode は通っているが値が
//! 想定外」の検出に弱くなる)。

use std::path::PathBuf;

use pretty_assertions::assert_eq;
use vozltop::model::{Buckets, VtsStatus};

fn load(name: &str) -> VtsStatus {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name);
    let raw =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str::<VtsStatus>(&raw)
        .unwrap_or_else(|e| panic!("decode {}: {e}", path.display()))
}

fn assert_empty_buckets(b: &Option<Buckets>, ctx: &str) {
    let b = b
        .as_ref()
        .unwrap_or_else(|| panic!("{ctx}: buckets is None"));
    assert!(b.msecs.is_empty(), "{ctx}: msecs should be empty");
    assert!(b.counters.is_empty(), "{ctx}: counters should be empty");
}

#[test]
fn decodes_no_histogram_fixture() {
    let status = load("no_histogram.json");

    assert_eq!(status.host_name, "54a48c223301");
    assert_eq!(status.nginx_version, "1.27.3");
    assert_eq!(status.module_version, "v0.2.5");
    assert_eq!(status.connections.requests, 12_513);
    assert!(status.load_msec > 0);
    assert!(status.now_msec >= status.load_msec);

    // 4 つの server zone がデコードされる ("*", "_", "api.example.test", "web.example.test")
    assert_eq!(status.server_zones.len(), 4);

    let api = status
        .server_zones
        .get("api.example.test")
        .expect("api zone present");
    assert_eq!(api.request_counter, 5_005);
    assert_eq!(api.responses.r2xx, 5_005);
    assert_eq!(api.responses.r5xx, 0);
    // histogram 未設定の zone でも requestBuckets フィールド自体は出るが、配列は空
    assert_empty_buckets(&api.request_buckets, "api.example.test");

    let web = status
        .server_zones
        .get("web.example.test")
        .expect("web zone present");
    assert_eq!(web.request_counter, 2_501);

    // upstream
    assert_eq!(status.upstream_zones.len(), 2);
    let api_upstreams = status
        .upstream_zones
        .get("backend_api")
        .expect("backend_api upstream present");
    assert_eq!(api_upstreams.len(), 2);
    assert_eq!(api_upstreams[0].server, "127.0.0.1:9001");
    assert_eq!(api_upstreams[0].request_counter, 2_503);
    assert_eq!(api_upstreams[1].server, "127.0.0.1:9002");
    assert_empty_buckets(
        &api_upstreams[0].request_buckets,
        "backend_api[0].request_buckets",
    );
    assert_empty_buckets(
        &api_upstreams[0].response_buckets,
        "backend_api[0].response_buckets",
    );

    // cache
    assert_eq!(status.cache_zones.len(), 1);
    let cache = status
        .cache_zones
        .get("demo_cache")
        .expect("demo_cache present");
    assert_eq!(cache.responses.hit, 2_500);
    assert_eq!(cache.responses.miss, 1);
    // cacheZones.responses には 1xx-5xx が存在しないが、Responses は #[serde(default)]
    // で 0 で埋まる
    assert_eq!(cache.responses.r2xx, 0);

    // shared
    assert_eq!(status.shared_zones.name, "vts");
    assert_eq!(status.shared_zones.used_node, 7);
}

#[test]
fn decodes_with_histogram_fixture() {
    let status = load("with_histogram.json");

    assert_eq!(status.host_name, "1bbc87663183");
    assert_eq!(status.nginx_version, "1.27.3");
    assert_eq!(status.module_version, "v0.2.5");

    // serverZones の requestBuckets が整数配列でデコードされる
    let api = status
        .server_zones
        .get("api.example.test")
        .expect("api zone present");
    assert_eq!(api.request_counter, 5_002);
    let api_buckets = api
        .request_buckets
        .as_ref()
        .expect("api.example.test should have requestBuckets");
    // nginx-with-histogram.conf の `vhost_traffic_status_histogram_buckets 0.005 0.01 0.05 0.1 0.5 1 5;`
    // を ms に換算した境界
    assert_eq!(api_buckets.msecs, vec![5, 10, 50, 100, 500, 1000, 5000]);
    assert_eq!(api_buckets.counters.len(), api_buckets.msecs.len());
    // u64 として正しく decode できているかを総和で確認
    let total: u64 = api_buckets.counters.iter().sum();
    assert_eq!(total, 35_014);

    // upstreamZones の requestBuckets も同じ境界
    let backend_api_0 = &status
        .upstream_zones
        .get("backend_api")
        .expect("backend_api upstream present")[0];
    assert_eq!(backend_api_0.server, "127.0.0.1:9001");
    let up_buckets = backend_api_0
        .request_buckets
        .as_ref()
        .expect("backend_api[0] should have requestBuckets");
    assert_eq!(up_buckets.msecs, vec![5, 10, 50, 100, 500, 1000, 5000]);
    assert_eq!(
        up_buckets.counters,
        vec![2_501, 2_501, 2_501, 2_501, 2_501, 2_501, 2_501]
    );

    // responseBuckets も同様に存在
    let up_response_buckets = backend_api_0
        .response_buckets
        .as_ref()
        .expect("backend_api[0] should have responseBuckets");
    assert_eq!(up_response_buckets.msecs.len(), 7);

    // cache zone
    let cache = status
        .cache_zones
        .get("demo_cache")
        .expect("demo_cache present");
    assert_eq!(cache.responses.hit, 3_001);
}

#[test]
fn buckets_round_trips_u64() {
    // u64 で正しく decode されることを `as_array` 経由ではなく型を使って確認する
    let status = load("with_histogram.json");
    let api = status.server_zones.get("api.example.test").unwrap();
    let buckets = api.request_buckets.as_ref().unwrap();

    // 数値が u64 に収まりきっている (overflow しない)
    for v in &buckets.msecs {
        let _: u64 = *v;
    }
    for v in &buckets.counters {
        let _: u64 = *v;
    }
}

#[test]
fn decodes_filter_zones() {
    // filterZones は `group -> key -> ServerZone` の 2 段ネスト (issue #45)。
    // initial.json / after_traffic.json に `country::*` グループ (US / JP) を
    // 追加してある。各 key は serverZones と同形の stats を持つ。
    let status = load("after_traffic.json");

    assert_eq!(status.filter_zones.len(), 1);
    let country = status
        .filter_zones
        .get("country::*")
        .expect("country::* filter group present");
    assert_eq!(country.len(), 2);

    let us = country.get("US").expect("US filter key present");
    assert_eq!(us.request_counter, 2_010);
    assert_eq!(us.responses.r2xx, 2_008);
    assert_eq!(us.responses.r5xx, 1);
    // filter key の stats は serverZones と同じく requestBuckets を持つ (空配列)。
    assert_empty_buckets(&us.request_buckets, "country::*/US");

    let jp = country.get("JP").expect("JP filter key present");
    assert_eq!(jp.request_counter, 1_001);
    assert_eq!(jp.responses.r5xx, 2);
}

#[test]
fn filter_zones_absent_decodes_to_empty_map() {
    // filterZones を持たない (= filter 未設定の nginx) fixture でも
    // `#[serde(default)]` で空 map になり decode が落ちないこと。
    let status = load("no_histogram.json");
    assert!(status.filter_zones.is_empty());
}

#[test]
fn upstream_servers_decode_as_vec() {
    // upstreamZones は HashMap<String, Vec<UpstreamServer>> 構造であることを確認
    let status = load("no_histogram.json");
    let api = status.upstream_zones.get("backend_api").unwrap();
    let web = status.upstream_zones.get("backend_web").unwrap();
    assert_eq!(api.len(), 2);
    assert_eq!(web.len(), 1);
    // server フィールドは "host:port" 形式
    for s in api.iter().chain(web.iter()) {
        assert!(
            s.server.contains(':'),
            "server should be host:port: {}",
            s.server
        );
    }
}
