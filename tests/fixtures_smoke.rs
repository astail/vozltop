//! 取得したフィクスチャ自体の健全性チェック。
//!
//! 詳細スキーマ (`VtsStatus` 型) の検証は issue #16 で `tests/deserialize.rs`
//! として追加する。ここでは「ファイルが存在し、有効な JSON で、必須のトップ
//! レベルキーが揃っている」ことだけを担保する。

use std::path::PathBuf;

use serde_json::Value;

fn fixture(name: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name);
    let raw =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

fn assert_top_level_keys(v: &Value, name: &str) {
    for key in [
        "hostName",
        "nginxVersion",
        "loadMsec",
        "nowMsec",
        "connections",
        "serverZones",
        "upstreamZones",
        "cacheZones",
    ] {
        assert!(
            v.get(key).is_some(),
            "{name}: top-level key `{key}` is missing"
        );
    }
}

#[test]
fn all_four_fixtures_exist_and_parse() {
    for name in [
        "initial.json",
        "after_traffic.json",
        "no_histogram.json",
        "with_histogram.json",
    ] {
        let v = fixture(name);
        assert_top_level_keys(&v, name);
    }
}

#[test]
fn no_histogram_has_empty_request_buckets() {
    for name in ["initial.json", "after_traffic.json", "no_histogram.json"] {
        let v = fixture(name);
        let buckets = &v["upstreamZones"]["backend_api"][0]["requestBuckets"];
        let msecs = buckets["msecs"].as_array().expect("msecs is array");
        let counters = buckets["counters"].as_array().expect("counters is array");
        assert!(msecs.is_empty(), "{name}: msecs should be empty");
        assert!(counters.is_empty(), "{name}: counters should be empty");
    }
}

#[test]
fn with_histogram_buckets_are_integer_arrays() {
    let v = fixture("with_histogram.json");
    let buckets = &v["upstreamZones"]["backend_api"][0]["requestBuckets"];
    let msecs = buckets["msecs"].as_array().expect("msecs is array");
    let counters = buckets["counters"].as_array().expect("counters is array");

    assert_eq!(
        msecs.len(),
        counters.len(),
        "msecs/counters length mismatch"
    );
    assert!(!msecs.is_empty(), "histogram should have buckets");

    for m in msecs {
        assert!(
            m.is_i64() || m.is_u64(),
            "msec value should be integer, got {m}"
        );
    }
    for c in counters {
        assert!(
            c.is_i64() || c.is_u64(),
            "counter value should be integer, got {c}"
        );
    }

    // tests/fixtures/setup/nginx-with-histogram.conf の
    // `vhost_traffic_status_histogram_buckets 0.005 0.01 0.05 0.1 0.5 1 5;`
    // を ms に換算した値。設定を変えた場合はここも必ず同期させること。
    let expected_msecs: Vec<i64> = vec![5, 10, 50, 100, 500, 1000, 5000];
    let actual_msecs: Vec<i64> = msecs.iter().map(|m| m.as_i64().unwrap()).collect();
    assert_eq!(actual_msecs, expected_msecs);
}

#[test]
fn cache_zones_have_hit_miss_counters() {
    for name in [
        "initial.json",
        "after_traffic.json",
        "no_histogram.json",
        "with_histogram.json",
    ] {
        let v = fixture(name);
        let responses = &v["cacheZones"]["demo_cache"]["responses"];
        for key in ["hit", "miss", "bypass", "expired", "stale"] {
            assert!(
                responses[key].is_u64() || responses[key].is_i64(),
                "{name}: cacheZones.demo_cache.responses.{key} should be integer"
            );
        }
    }
}

#[test]
fn initial_and_after_traffic_share_host_for_derived_metrics() {
    let initial = fixture("initial.json");
    let after = fixture("after_traffic.json");
    assert_eq!(
        initial["hostName"], after["hostName"],
        "initial と after_traffic は同一ホスト由来のペアでなければならない"
    );
    let initial_now = initial["nowMsec"].as_i64().expect("nowMsec is integer");
    let after_now = after["nowMsec"].as_i64().expect("nowMsec is integer");
    assert!(
        after_now > initial_now,
        "after_traffic の nowMsec は initial より後でなければならない"
    );
}
