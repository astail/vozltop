//! `compute(prev, now)` を `initial.json` / `after_traffic.json` の実フィクスチャに
//! 当てて、期待される RPS / BW/s / 2xx% / 5xx% / cache hit% を検証する。
//!
//! 期待値は fixture の `nowMsec` とカウンタ値から算出した固定値で比較する。
//! `tests/fixtures/README.md` の手順でフィクスチャを再取得した場合は本ファイル
//! 中の固定値も同時に更新する必要がある (deserialize テストと同じポリシー)。

use std::path::PathBuf;

use vozltop::model::VtsStatus;
use vozltop::state::{compute, DerivedSnapshot};

const EPSILON: f64 = 1e-6;

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

fn snapshots() -> (VtsStatus, VtsStatus) {
    (load("initial.json"), load("after_traffic.json"))
}

fn assert_close(actual: f64, expected: f64, ctx: &str) {
    assert!(
        (actual - expected).abs() < EPSILON,
        "{ctx}: expected {expected}, got {actual}"
    );
}

fn assert_close_some(actual: Option<f64>, expected: f64, ctx: &str) {
    let v = actual.unwrap_or_else(|| panic!("{ctx}: expected Some({expected}), got None"));
    assert_close(v, expected, ctx);
}

fn run() -> DerivedSnapshot {
    let (prev, now) = snapshots();
    compute(&prev, &now).expect("dt > 0 for fixture pair")
}

#[test]
fn dt_ms_matches_fixture_nowmsec_delta() {
    let d = run();
    // 1779411793897 - 1779411777213 = 16684
    assert_eq!(d.dt_ms, 16_684);
}

#[test]
fn server_zone_api_example_rates_match_expected() {
    let d = run();
    let api = d
        .server
        .get("api.example.test")
        .expect("api.example.test present");

    // delta_rc = 3008 - 3 = 3005, dt = 16.684s → rps = 3005 / 16.684
    assert_close(api.rates.rps, 3005.0 / 16.684, "api.example.test rps");
    // delta_in = 252672 - 237 = 252_435
    assert_close(
        api.rates.bw_in_per_sec,
        252_435.0 / 16.684,
        "api.example.test bw_in_per_sec",
    );
    // delta_out = 508392 - 522 = 507_870
    assert_close(
        api.rates.bw_out_per_sec,
        507_870.0 / 16.684,
        "api.example.test bw_out_per_sec",
    );

    // 全て 2xx → 2xx% = 100, それ以外 0
    assert_close_some(api.ratios.r2xx_pct, 100.0, "api.example.test 2xx%");
    assert_close_some(api.ratios.r1xx_pct, 0.0, "api.example.test 1xx%");
    assert_close_some(api.ratios.r3xx_pct, 0.0, "api.example.test 3xx%");
    assert_close_some(api.ratios.r4xx_pct, 0.0, "api.example.test 4xx%");
    assert_close_some(api.ratios.r5xx_pct, 0.0, "api.example.test 5xx%");
}

#[test]
fn server_zone_web_example_rates_match_expected() {
    let d = run();
    let web = d
        .server
        .get("web.example.test")
        .expect("web.example.test present");

    // delta_rc = 2503 - 3 = 2500
    assert_close(web.rates.rps, 2500.0 / 16.684, "web.example.test rps");
    // delta_in = 213237 - 237 = 213_000
    assert_close(
        web.rates.bw_in_per_sec,
        213_000.0 / 16.684,
        "web.example.test bw_in_per_sec",
    );
}

#[test]
fn server_zone_aggregate_star_present() {
    // "*" は VTS が出す zone 横断合計エントリ。compute() は zone 名で区別せず
    // 等しく算出する (zone 横断 RPS が必要なら呼び出し元で別途集計する想定)。
    let d = run();
    let total = d.server.get("*").expect("'*' aggregate zone present");
    assert_close(total.rates.rps, (8521.0 - 10.0) / 16.684, "'*' rps");
}

#[test]
fn upstream_key_is_group_slash_server() {
    let d = run();
    // backend_api/127.0.0.1:9001: delta_rc = 1504 - 2 = 1502
    let key1 = "backend_api/127.0.0.1:9001";
    let u1 = d.upstream.get(key1).unwrap_or_else(|| {
        panic!(
            "upstream key '{key1}' missing; got: {:?}",
            d.upstream.keys()
        )
    });
    assert_close(u1.rates.rps, 1502.0 / 16.684, "{key1} rps");

    // backend_api/127.0.0.1:9002: delta_rc = 1504 - 1 = 1503
    let key2 = "backend_api/127.0.0.1:9002";
    let u2 = d.upstream.get(key2).expect("backend_api/9002 present");
    assert_close(u2.rates.rps, 1503.0 / 16.684, "{key2} rps");
}

#[test]
fn upstream_with_no_delta_has_zero_rates_and_none_ratios() {
    // backend_web/127.0.0.1:9001 はトラフィック生成中に叩かれていないので
    // delta = 0 → RPS = 0, ratios = None (分母 0)
    let d = run();
    let key = "backend_web/127.0.0.1:9001";
    let u = d.upstream.get(key).expect("backend_web/9001 present");
    assert_close(u.rates.rps, 0.0, "{key} rps");
    assert_close(u.rates.bw_in_per_sec, 0.0, "{key} bw_in_per_sec");
    assert_close(u.rates.bw_out_per_sec, 0.0, "{key} bw_out_per_sec");
    assert!(
        u.ratios.r2xx_pct.is_none(),
        "{key} ratios should be None when no traffic delta"
    );
}

#[test]
fn cache_zone_bandwidth_and_hit_ratio() {
    let d = run();
    let cache = d.cache.get("demo_cache").expect("demo_cache present");
    // delta_in = 168237 - 237 = 168_000, delta_out = 338522 - 522 = 338_000
    assert_close(
        cache.bw_in_per_sec,
        168_000.0 / 16.684,
        "demo_cache bw_in_per_sec",
    );
    assert_close(
        cache.bw_out_per_sec,
        338_000.0 / 16.684,
        "demo_cache bw_out_per_sec",
    );

    // 累積カウンタ: hit=2002, miss=1, 残りは 0 → hit% = 2002 / 2003 * 100
    let expected_hit_pct = 2002.0 * 100.0 / 2003.0;
    assert_close_some(cache.hit_pct, expected_hit_pct, "demo_cache hit_pct");
}

#[test]
fn counter_reset_returns_none() {
    // nginx 再起動を模擬: nowMsec が前回より小さい snapshot を後段に置く
    let prev = load("after_traffic.json");
    let now = load("initial.json");
    assert!(
        compute(&prev, &now).is_none(),
        "nowMsec regression should return None"
    );
}

#[test]
fn identical_snapshot_returns_none() {
    // 同一 tick (dt = 0) は派生計算スキップ
    let s = load("initial.json");
    assert!(compute(&s, &s).is_none());
}
