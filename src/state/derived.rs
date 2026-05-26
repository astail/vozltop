//! 派生メトリクス (RPS / BW per sec / status ratio / cache hit%) の算出。
//!
//! 2 つの `VtsStatus` snapshot を受けて、zone 毎の差分メトリクスを返す。
//!
//! - dt_ms = `now.now_msec - prev.now_msec`
//! - dt_ms <= 0 (= nginx 再起動 or 同一 tick) なら計算をスキップして `None` を返す
//! - 各 zone について `delta(counter) / dt_secs` で per-sec 値を算出
//! - upstream は `"group/host:port"` をキーに 1 server = 1 エントリ
//! - cache は request_counter を持たないため hit% と BW のみ
//!
//! UI 側では「分母 0 のとき `—` 表示」「histogram 未設定 zone は別群でソート」
//! など `Option` を文字列に落とす責務を担う。本モジュールは数値層のみ。

use std::collections::HashMap;

use crate::model::{Responses, VtsStatus};

/// 1 zone あたりの rate 系メトリクス。
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ZoneRates {
    /// requests/sec。
    pub rps: f64,
    /// inbound bytes/sec。
    pub bw_in_per_sec: f64,
    /// outbound bytes/sec。
    pub bw_out_per_sec: f64,
}

/// HTTP status カテゴリの比率 (パーセント、0.0〜100.0)。
///
/// 分母 (= 1xx〜5xx の差分合計) が 0 のときは全フィールド `None`。
/// 値が `Some` の場合は 5 フィールドの合計が 100.0 (浮動小数誤差の範囲内) になる。
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct StatusRatios {
    pub r1xx_pct: Option<f64>,
    pub r2xx_pct: Option<f64>,
    pub r3xx_pct: Option<f64>,
    pub r4xx_pct: Option<f64>,
    pub r5xx_pct: Option<f64>,
}

/// serverZones 1 zone ぶんの派生値。
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ServerDerived {
    pub rates: ZoneRates,
    pub ratios: StatusRatios,
}

/// upstreamZones 1 server ぶんの派生値。キーは `"group/host:port"`。
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct UpstreamDerived {
    pub rates: ZoneRates,
    pub ratios: StatusRatios,
}

/// cacheZones 1 zone ぶんの派生値。
///
/// cache zone は `requestCounter` を持たないため RPS は計算しない (UI は
/// `—` を表示する想定)。`hit_pct` は累積カウンタからの絶対値で、`Option` は
/// 分母 0 (まだ何もキャッシュ系応答が発生していない) のときに `None`。
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CacheDerived {
    pub bw_in_per_sec: f64,
    pub bw_out_per_sec: f64,
    pub hit_pct: Option<f64>,
}

/// 1 tick ぶんの派生メトリクス全体。
///
/// `dt_ms = 0` のままになることはない (`compute` 内で 0/負を弾いて `None` を
/// 返すため)。`derive(Default)` は `Snapshot { derived: DerivedSnapshot::default() }`
/// のような placeholder 初期化用に残す (issue #20 の history.rs 経由)。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DerivedSnapshot {
    /// 2 snapshot 間の経過ミリ秒。
    pub dt_ms: u64,
    /// serverZones の派生値。キー = zone 名。
    pub server: HashMap<String, ServerDerived>,
    /// upstreamZones の派生値。キー = `"group/host:port"`。
    pub upstream: HashMap<String, UpstreamDerived>,
    /// cacheZones の派生値。キー = cache zone 名。
    pub cache: HashMap<String, CacheDerived>,
}

/// 2 つの `VtsStatus` snapshot から派生メトリクスを算出する。
///
/// 戻り値が `None` になる条件:
/// - `now.now_msec <= prev.now_msec` (= 同一 tick または nginx 再起動による
///   `nowMsec` の単調性違反)
///
/// 戻り値が `Some` のときの各 zone の扱い:
/// - `now` に存在し `prev` にも存在する zone のみ算出 (新規 zone は次回 tick
///   から差分が取れるようになるので 1 tick だけ skip)
/// - 個別カウンタの逆行 (zone 単位のリセット等) は `saturating_sub` で 0 扱い
pub fn compute(prev: &VtsStatus, now: &VtsStatus) -> Option<DerivedSnapshot> {
    let dt_ms_i128 = now.now_msec as i128 - prev.now_msec as i128;
    if dt_ms_i128 <= 0 {
        return None;
    }
    let dt_ms = dt_ms_i128 as u64;
    let dt_secs = dt_ms as f64 / 1000.0;

    let mut server = HashMap::with_capacity(now.server_zones.len());
    for (name, now_zone) in &now.server_zones {
        let Some(prev_zone) = prev.server_zones.get(name) else {
            continue;
        };
        let rates = ZoneRates {
            rps: per_sec(now_zone.request_counter, prev_zone.request_counter, dt_secs),
            bw_in_per_sec: per_sec(now_zone.in_bytes, prev_zone.in_bytes, dt_secs),
            bw_out_per_sec: per_sec(now_zone.out_bytes, prev_zone.out_bytes, dt_secs),
        };
        let ratios = status_ratios(&now_zone.responses, &prev_zone.responses);
        server.insert(name.clone(), ServerDerived { rates, ratios });
    }

    let mut upstream = HashMap::new();
    for (group, now_servers) in &now.upstream_zones {
        let Some(prev_servers) = prev.upstream_zones.get(group) else {
            continue;
        };
        for now_server in now_servers {
            let Some(prev_server) = prev_servers.iter().find(|s| s.server == now_server.server)
            else {
                continue;
            };
            let key = format!("{group}/{}", now_server.server);
            let rates = ZoneRates {
                rps: per_sec(
                    now_server.request_counter,
                    prev_server.request_counter,
                    dt_secs,
                ),
                bw_in_per_sec: per_sec(now_server.in_bytes, prev_server.in_bytes, dt_secs),
                bw_out_per_sec: per_sec(now_server.out_bytes, prev_server.out_bytes, dt_secs),
            };
            let ratios = status_ratios(&now_server.responses, &prev_server.responses);
            upstream.insert(key, UpstreamDerived { rates, ratios });
        }
    }

    let mut cache = HashMap::with_capacity(now.cache_zones.len());
    for (name, now_zone) in &now.cache_zones {
        let Some(prev_zone) = prev.cache_zones.get(name) else {
            continue;
        };
        let bw_in_per_sec = per_sec(now_zone.in_bytes, prev_zone.in_bytes, dt_secs);
        let bw_out_per_sec = per_sec(now_zone.out_bytes, prev_zone.out_bytes, dt_secs);
        let hit_pct = cache_hit_pct(&now_zone.responses);
        cache.insert(
            name.clone(),
            CacheDerived {
                bw_in_per_sec,
                bw_out_per_sec,
                hit_pct,
            },
        );
    }

    Some(DerivedSnapshot {
        dt_ms,
        server,
        upstream,
        cache,
    })
}

fn per_sec(now: u64, prev: u64, dt_secs: f64) -> f64 {
    // saturating_sub で個別カウンタの逆行を 0 として吸収する。
    // 逆行が発生するのはほぼ nginx 再起動だが、その場合は呼び出し元で
    // dt_ms <= 0 として弾かれている前提。万一 nowMsec が前進したまま zone
    // 単位のリセット (config reload 等) が起こったときも panic させない。
    let delta = now.saturating_sub(prev) as f64;
    delta / dt_secs
}

fn status_ratios(now: &Responses, prev: &Responses) -> StatusRatios {
    let d1 = now.r1xx.saturating_sub(prev.r1xx);
    let d2 = now.r2xx.saturating_sub(prev.r2xx);
    let d3 = now.r3xx.saturating_sub(prev.r3xx);
    let d4 = now.r4xx.saturating_sub(prev.r4xx);
    let d5 = now.r5xx.saturating_sub(prev.r5xx);
    let sum = d1 + d2 + d3 + d4 + d5;
    if sum == 0 {
        return StatusRatios::default();
    }
    let s = sum as f64;
    StatusRatios {
        r1xx_pct: Some(d1 as f64 * 100.0 / s),
        r2xx_pct: Some(d2 as f64 * 100.0 / s),
        r3xx_pct: Some(d3 as f64 * 100.0 / s),
        r4xx_pct: Some(d4 as f64 * 100.0 / s),
        r5xx_pct: Some(d5 as f64 * 100.0 / s),
    }
}

fn cache_hit_pct(r: &Responses) -> Option<f64> {
    // VTS の cache zone responses は累積カウンタ。差分ではなく絶対値で hit%
    // を出す方が運用上「キャッシュ寿命全体でのヒット率」として読みやすい。
    // 差分ベースが必要になったら呼び出し元で prev/now を受け取る版に拡張する。
    let denom =
        r.hit + r.miss + r.bypass + r.expired + r.stale + r.updating + r.revalidated + r.scarce;
    if denom == 0 {
        None
    } else {
        Some(r.hit as f64 * 100.0 / denom as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::VtsStatus;

    fn snapshot(now_msec: u64) -> VtsStatus {
        let raw = serde_json::json!({
            "hostName": "h", "nginxVersion": "1", "moduleVersion": "v",
            "loadMsec": 0u64, "nowMsec": now_msec,
            "connections": {
                "active": 0, "reading": 0, "writing": 0,
                "waiting": 0, "accepted": 0, "handled": 0, "requests": 0
            },
        });
        serde_json::from_value(raw).unwrap()
    }

    #[test]
    fn returns_none_when_dt_is_zero() {
        let s = snapshot(1_000);
        assert!(compute(&s, &s).is_none());
    }

    #[test]
    fn returns_none_when_now_is_before_prev() {
        // nginx 再起動で nowMsec が巻き戻ったケース。
        let prev = snapshot(1_000_000);
        let now = snapshot(500);
        assert!(compute(&prev, &now).is_none());
    }

    #[test]
    fn computes_dt_ms_when_now_is_after_prev() {
        let prev = snapshot(1_000);
        let now = snapshot(2_500);
        let d = compute(&prev, &now).expect("dt > 0");
        assert_eq!(d.dt_ms, 1_500);
        assert!(d.server.is_empty());
        assert!(d.upstream.is_empty());
        assert!(d.cache.is_empty());
    }

    #[test]
    fn status_ratios_none_when_no_traffic_in_window() {
        let r_prev = Responses {
            r2xx: 100,
            ..Responses::default()
        };
        let r_now = Responses {
            r2xx: 100,
            ..Responses::default()
        };
        let ratios = status_ratios(&r_now, &r_prev);
        assert!(ratios.r2xx_pct.is_none());
    }

    #[test]
    fn status_ratios_sum_to_100_with_mixed_codes() {
        let r_prev = Responses::default();
        let r_now = Responses {
            r2xx: 80,
            r4xx: 15,
            r5xx: 5,
            ..Responses::default()
        };
        let ratios = status_ratios(&r_now, &r_prev);
        let sum = ratios.r1xx_pct.unwrap()
            + ratios.r2xx_pct.unwrap()
            + ratios.r3xx_pct.unwrap()
            + ratios.r4xx_pct.unwrap()
            + ratios.r5xx_pct.unwrap();
        assert!((sum - 100.0).abs() < 1e-9, "sum should be 100.0, got {sum}");
        assert!((ratios.r2xx_pct.unwrap() - 80.0).abs() < 1e-9);
        assert!((ratios.r5xx_pct.unwrap() - 5.0).abs() < 1e-9);
    }

    #[test]
    fn per_sec_handles_counter_regression() {
        // 個別カウンタが逆行しても 0 を返し panic しない。
        assert_eq!(per_sec(5, 10, 1.0), 0.0);
    }

    #[test]
    fn cache_hit_pct_none_when_denominator_zero() {
        let r = Responses::default();
        assert!(cache_hit_pct(&r).is_none());
    }

    #[test]
    fn cache_hit_pct_computed_from_absolute_counters() {
        let r = Responses {
            hit: 99,
            miss: 1,
            ..Responses::default()
        };
        let pct = cache_hit_pct(&r).unwrap();
        assert!((pct - 99.0).abs() < 1e-9, "expected 99.0%, got {pct}");
    }
}
