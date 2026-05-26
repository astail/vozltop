//! `requestBuckets` から p50 / p95 / p99 等を線形補間で算出する。
//!
//! ## カウンタの意味
//!
//! nginx-module-vts の `requestBuckets.counters[i]` は「リクエスト latency が
//! `msecs[i]` ms 以下である件数 (累積)」であり、bucket をまたいで cumulative。
//! 例: `msecs = [5, 10, 50, ...]` で 1 件の 3ms リクエストが入ると、`counters`
//! は `[1, 1, 1, ...]` のように全 bucket がインクリメントされる
//! (= "<= bucket boundary" の累積)。`tests/deserialize.rs` の
//! `with_histogram_buckets_are_integer_arrays` で sum=7×5002 になるのが
//! その挙動の証拠。
//!
//! ## アルゴリズム
//!
//! 2 つの snapshot (`prev` / `now`) で同じ bucket の差分を取ると、window 内の
//! "msec ≤ msecs[i] の累積件数" `D[i]` が得られる。`total = D[n-1]` を全件数と
//! みなし、`target = total * p` の bucket を二分でなく線形に scan して見つけ、
//! `[msecs[i-1], msecs[i]]` 区間内の線形補間で値を返す。`i = 0` のとき下限は
//! 0 ms とみなす (nginx-vts の bucket は (0, msecs[0]] の半開区間)。
//!
//! ## エッジケース
//!
//! - `prev.msecs != now.msecs` (histogram 再設定 / shape 不一致) → `NoData`
//! - `now.msecs` が空 (histogram 未設定 zone) → `NoData`
//!   (呼び出し側で [`average_fallback`] を使う想定)
//! - window 内の差分合計が 0 → `NoData`
//! - 個別 bucket カウンタの逆行 → `saturating_sub` で 0 扱い
//! - `target` を満たす bucket が見つからない (p > 1.0 等の異常系) → `Overflow`
//! - `target` を満たす bucket の `bucket_count == 0`
//!   (浮動小数点丸めの境界ケース) → 下限値を返す
//!
//! ## ソート規約
//!
//! UI 側で p95 列ソートする際の並び順は [`compare_for_sort`] を使う:
//! 1. `Value` / `Overflow` (= histogram あり群)
//! 2. `Average` (= histogram 未設定 zone のフォールバック)
//! 3. `NoData`
//!
//! この tier ベースの 2 段ソートは "histogram あり zone と平均値 zone を
//! 混在ソートすると値の意味が違って誤解を生む" 設計判断 (`docs/DESIGN.md`) に
//! 由来する。

use std::cmp::Ordering;

use crate::model::Buckets;

/// パーセンタイル算出結果。
///
/// `f64` の単位は ミリ秒。`Overflow` のみ `u64` を保持しているのは
/// "上限 (= 最終 bucket の msec)" を整数値で明示するため。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PercentileResult {
    /// histogram からの線形補間で得た値 (ms)。
    Value(f64),
    /// 全リクエストが histogram の最終 bucket 上限を超えた (= `>Nms` 表示)。
    /// 内部値は `msecs.last()`。実運用ではほぼ p > 1.0 の異常系のみ到達する。
    Overflow(u64),
    /// histogram 未設定 zone 向けの fallback。`request_msec` (区間平均、ms)。
    /// UI は `~Nms` プレフィックスで表示する。
    Average(f64),
    /// データ不足 (空 histogram / shape 不一致 / window 内 0 件)。
    NoData,
}

impl PercentileResult {
    /// ソート時に "どの群か" を決める tier (小さい方が先頭)。
    ///
    /// - 0: `Value` / `Overflow` (histogram あり群)
    /// - 1: `Average` (histogram なし fallback)
    /// - 2: `NoData`
    pub fn sort_tier(&self) -> u8 {
        match self {
            PercentileResult::Value(_) | PercentileResult::Overflow(_) => 0,
            PercentileResult::Average(_) => 1,
            PercentileResult::NoData => 2,
        }
    }

    /// 同 tier 内で数値比較に使う値。`NoData` のみ `None`。
    /// `Overflow(n)` は `n as f64` を返す (= 最終 bucket 上限 ms)。
    pub fn sort_value(&self) -> Option<f64> {
        match self {
            PercentileResult::Value(v) | PercentileResult::Average(v) => Some(*v),
            PercentileResult::Overflow(max) => Some(*max as f64),
            PercentileResult::NoData => None,
        }
    }
}

/// "histogram あり群 → Average 群 → NoData 群" の安定化ソート用比較。
///
/// tier 内では数値の **昇順**。降順表示が必要な UI では、tier が一致する
/// ケースの戻り値だけを反転する形で使う:
/// ```rust,ignore
/// rows.sort_by(|a, b| {
///     a.p95.sort_tier().cmp(&b.p95.sort_tier()).then_with(|| {
///         let asc = compare_for_sort(&a.p95, &b.p95);
///         if descending { asc.reverse() } else { asc }
///     })
/// });
/// ```
pub fn compare_for_sort(a: &PercentileResult, b: &PercentileResult) -> Ordering {
    a.sort_tier()
        .cmp(&b.sort_tier())
        .then_with(|| match (a.sort_value(), b.sort_value()) {
            (Some(x), Some(y)) => x.partial_cmp(&y).unwrap_or(Ordering::Equal),
            (None, None) => Ordering::Equal,
            // sort_value が None になるのは NoData のみで、tier が一致している前提だと
            // 両方とも NoData。ここに来る両 None は上の分岐に倒される。残りは
            // 不整合 (tier 同じなのに片方だけ None) なので Equal で済ます。
            _ => Ordering::Equal,
        })
}

/// histogram 差分から `p` パーセンタイル (ms, f64) を算出する。
///
/// `p` は `0.0..=1.0` を想定。範囲外を渡すと `Overflow` (p > 1.0) または
/// 先頭 bucket の補間値 (p < 0.0 で target が負) を返す。
pub fn percentile(prev: &Buckets, now: &Buckets, p: f64) -> PercentileResult {
    // Shape mismatch: msec 境界の配列が違う / 長さがずれている。histogram の
    // バケット設定が runtime で変わった or 受信間で nginx config reload が
    // 走った扱いで NoData を返す。
    if prev.msecs != now.msecs
        || prev.counters.len() != now.counters.len()
        || now.msecs.len() != now.counters.len()
    {
        return PercentileResult::NoData;
    }

    // histogram 未設定 zone (`requestBuckets: {msecs:[], counters:[]}`)。
    if now.msecs.is_empty() {
        return PercentileResult::NoData;
    }

    // D[i] = window 内で msec ≤ msecs[i] の累積件数。
    let d: Vec<u64> = now
        .counters
        .iter()
        .zip(prev.counters.iter())
        .map(|(n_i, p_i)| n_i.saturating_sub(*p_i))
        .collect();
    // total = D[n-1] = window 内の histogram 計上件数の合計。
    // histogram の最終 bucket 上限を超えた request はここに乗らない点に注意
    // (request_counter との差分で別途検知する責務は呼び出し側)。
    let total = *d.last().expect("n > 0 was checked above");
    if total == 0 {
        return PercentileResult::NoData;
    }

    let target = total as f64 * p;

    let mut cum_prev: u64 = 0;
    for (i, &d_i) in d.iter().enumerate() {
        if d_i as f64 >= target {
            let bucket_count = d_i - cum_prev;
            let lower = if i == 0 { 0 } else { now.msecs[i - 1] };
            let upper = now.msecs[i];
            if bucket_count == 0 {
                // 浮動小数の境界ケース: target が D[i-1] と D[i] の両方に等しく
                // ぎりぎり後方の bucket が選ばれる場合に発生し得る。仕様通り
                // 下限を返す (補間不能の安全側フォールバック)。
                return PercentileResult::Value(lower as f64);
            }
            let interp = (target - cum_prev as f64) / bucket_count as f64;
            let value = lower as f64 + upper.saturating_sub(lower) as f64 * interp;
            return PercentileResult::Value(value);
        }
        cum_prev = d_i;
    }

    // ここに来るのは target > total = D[n-1] のみ。`p` の範囲が正常 (≤ 1.0)
    // なら mathematically 到達しないが、p > 1.0 や浮動小数の上振れで稀に来る。
    PercentileResult::Overflow(*now.msecs.last().expect("n > 0 was checked above"))
}

/// histogram 未設定 zone 向けの fallback。`request_msec` (= 区間平均 ms) を
/// `Average` でラップして返す。UI 側は `~Nms` プレフィックスで表示する想定。
///
/// `percentile()` と組み合わせるときの典型呼び出し:
/// ```rust,ignore
/// let p95 = if zone.request_buckets.as_ref().map_or(true, |b| b.msecs.is_empty()) {
///     average_fallback(zone.request_msec)
/// } else {
///     percentile(prev_buckets, now_buckets, 0.95)
/// };
/// ```
pub fn average_fallback(request_msec: u64) -> PercentileResult {
    PercentileResult::Average(request_msec as f64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Buckets;

    fn buckets(msecs: &[u64], counters: &[u64]) -> Buckets {
        Buckets {
            msecs: msecs.to_vec(),
            counters: counters.to_vec(),
        }
    }

    fn assert_close(actual: PercentileResult, expected_ms: f64, ctx: &str) {
        match actual {
            PercentileResult::Value(v) => assert!(
                (v - expected_ms).abs() < 1.0,
                "{ctx}: expected {expected_ms} ms (±1), got Value({v})"
            ),
            other => panic!("{ctx}: expected Value, got {other:?}"),
        }
    }

    #[test]
    fn nodata_when_msecs_arrays_differ() {
        let p = buckets(&[5, 10], &[0, 0]);
        let n = buckets(&[5, 20], &[1, 1]);
        assert_eq!(percentile(&p, &n, 0.95), PercentileResult::NoData);
    }

    #[test]
    fn nodata_when_msecs_lengths_differ() {
        let p = buckets(&[5, 10], &[0, 0]);
        let n = buckets(&[5, 10, 50], &[1, 1, 1]);
        assert_eq!(percentile(&p, &n, 0.95), PercentileResult::NoData);
    }

    #[test]
    fn nodata_when_counters_length_mismatches_msecs() {
        let p = buckets(&[5, 10], &[0]);
        let n = buckets(&[5, 10], &[1]);
        assert_eq!(percentile(&p, &n, 0.95), PercentileResult::NoData);
    }

    #[test]
    fn nodata_when_histogram_is_unset() {
        let p = buckets(&[], &[]);
        let n = buckets(&[], &[]);
        assert_eq!(percentile(&p, &n, 0.95), PercentileResult::NoData);
    }

    #[test]
    fn nodata_when_no_traffic_in_window() {
        let p = buckets(&[5, 10, 50], &[100, 100, 100]);
        let n = buckets(&[5, 10, 50], &[100, 100, 100]);
        assert_eq!(percentile(&p, &n, 0.95), PercentileResult::NoData);
    }

    #[test]
    fn counter_regression_in_single_bucket_is_saturated() {
        // 個別 bucket 逆行は saturating_sub で 0 扱い。total が正なら算出継続。
        let p = buckets(&[5, 10, 50], &[10, 0, 0]);
        let n = buckets(&[5, 10, 50], &[5, 5, 5]); // bucket 0 が逆行
                                                   // D[0] = saturating_sub(5, 10) = 0
                                                   // D[1] = 5 - 0 = 5
                                                   // D[2] = 5 - 0 = 5
                                                   // total = 5, p95: target = 4.75
                                                   // i=0: D[0]=0 < 4.75. cum_prev=0.
                                                   // i=1: D[1]=5 >= 4.75. bucket_count = 5-0 = 5. lower=msecs[0]=5, upper=10.
                                                   //      interp = (4.75 - 0) / 5 = 0.95. value = 5 + 5*0.95 = 9.75.
        assert_close(
            percentile(&p, &n, 0.95),
            9.75,
            "saturating counter regression",
        );
    }

    #[test]
    fn overflow_when_p_greater_than_one() {
        let p = buckets(&[5, 10, 50], &[0, 0, 0]);
        let n = buckets(&[5, 10, 50], &[5, 8, 10]);
        // p > 1.0 → target > total = 10
        assert_eq!(percentile(&p, &n, 1.5), PercentileResult::Overflow(50));
    }

    #[test]
    fn value_falls_in_first_bucket_interpolates_from_zero() {
        // D = [50, 100], msecs = [10, 50]. total = 100. p50: target = 50.
        // i=0: D[0]=50 >= 50. cum_prev=0. lower=0, upper=10. bucket_count=50.
        //      interp = (50-0)/50 = 1.0. value = 0 + 10*1.0 = 10.0
        let p = buckets(&[10, 50], &[0, 0]);
        let n = buckets(&[10, 50], &[50, 100]);
        assert_close(
            percentile(&p, &n, 0.5),
            10.0,
            "p50 at exact bucket boundary",
        );
    }

    #[test]
    fn value_in_later_bucket_uses_cumulative_diff() {
        // 独立計算 (Python 相当):
        // msecs   = [5, 10, 50, 100, 500]
        // prev    = [0,  0,  0,   0,   0]
        // now     = [10, 50, 100, 150, 200]
        // D       = [10, 50, 100, 150, 200]
        // total = 200
        // p50: target = 100. i=2: D[2]=100 >= 100. cum_prev = D[1] = 50.
        //   bucket_count = 100-50=50. lower=10, upper=50. interp=(100-50)/50=1.0.
        //   value = 10 + 40*1.0 = 50.0
        // p95: target = 190. i=4: D[4]=200 >= 190. cum_prev = D[3] = 150.
        //   bucket_count = 50. lower=100, upper=500. interp=(190-150)/50=0.8.
        //   value = 100 + 400*0.8 = 420.0
        // p99: target = 198. i=4: D[4]=200 >= 198. cum_prev = 150.
        //   interp = (198-150)/50 = 0.96. value = 100 + 400*0.96 = 484.0
        let p = buckets(&[5, 10, 50, 100, 500], &[0, 0, 0, 0, 0]);
        let n = buckets(&[5, 10, 50, 100, 500], &[10, 50, 100, 150, 200]);
        assert_close(percentile(&p, &n, 0.5), 50.0, "p50 mid distribution");
        assert_close(percentile(&p, &n, 0.95), 420.0, "p95 mid distribution");
        assert_close(percentile(&p, &n, 0.99), 484.0, "p99 mid distribution");
    }

    #[test]
    fn p_zero_returns_lowest_bound() {
        let p = buckets(&[5, 10, 50], &[0, 0, 0]);
        let n = buckets(&[5, 10, 50], &[10, 20, 30]);
        // p=0 → target=0. i=0: D[0]=10 >= 0. bucket_count=10. interp=0/10=0.
        // value = 0 + 5*0 = 0.
        assert_close(percentile(&p, &n, 0.0), 0.0, "p=0");
    }

    #[test]
    fn p_one_returns_topmost_bucket_msec() {
        let p = buckets(&[5, 10, 50], &[0, 0, 0]);
        let n = buckets(&[5, 10, 50], &[10, 20, 30]);
        // p=1.0 → target=30. i=2: D[2]=30 >= 30. cum_prev=D[1]=20.
        //   bucket_count=10. interp=(30-20)/10=1.0. value = 10 + 40*1.0 = 50.0
        assert_close(percentile(&p, &n, 1.0), 50.0, "p=1.0 hits last bucket msec");
    }

    #[test]
    fn average_fallback_wraps_request_msec() {
        assert_eq!(average_fallback(42), PercentileResult::Average(42.0));
    }

    #[test]
    fn sort_tier_orders_value_average_nodata() {
        assert_eq!(PercentileResult::Value(100.0).sort_tier(), 0);
        assert_eq!(PercentileResult::Overflow(5000).sort_tier(), 0);
        assert_eq!(PercentileResult::Average(50.0).sort_tier(), 1);
        assert_eq!(PercentileResult::NoData.sort_tier(), 2);
    }

    #[test]
    fn compare_for_sort_keeps_value_group_before_average_group() {
        // たとえ Value の値が Average より大きくても、tier 優先で Value が先に来る
        let big_value = PercentileResult::Value(9999.0);
        let small_average = PercentileResult::Average(1.0);
        assert_eq!(compare_for_sort(&big_value, &small_average), Ordering::Less);
        assert_eq!(
            compare_for_sort(&small_average, &big_value),
            Ordering::Greater
        );
    }

    #[test]
    fn compare_for_sort_orders_within_value_group_numerically() {
        let small = PercentileResult::Value(10.0);
        let big = PercentileResult::Value(100.0);
        assert_eq!(compare_for_sort(&small, &big), Ordering::Less);
        assert_eq!(compare_for_sort(&big, &small), Ordering::Greater);
        assert_eq!(compare_for_sort(&small, &small), Ordering::Equal);
    }

    #[test]
    fn compare_for_sort_orders_overflow_as_max_msec() {
        // Overflow(5000) は Value(4999) より大きい
        let overflow = PercentileResult::Overflow(5000);
        let value = PercentileResult::Value(4999.0);
        assert_eq!(compare_for_sort(&value, &overflow), Ordering::Less);
    }

    #[test]
    fn compare_for_sort_orders_nodata_last() {
        let any_value = PercentileResult::Value(0.0);
        let any_average = PercentileResult::Average(0.0);
        let nodata = PercentileResult::NoData;
        assert_eq!(compare_for_sort(&any_value, &nodata), Ordering::Less);
        assert_eq!(compare_for_sort(&any_average, &nodata), Ordering::Less);
        assert_eq!(compare_for_sort(&nodata, &nodata), Ordering::Equal);
    }

    #[test]
    fn full_sort_via_compare_for_sort_produces_expected_order() {
        // 入力: value(50), nodata, average(200), value(10), overflow(5000), average(5)
        // 期待順 (昇順): value(10), value(50), overflow(5000), average(5), average(200), nodata
        let mut xs = vec![
            PercentileResult::Value(50.0),
            PercentileResult::NoData,
            PercentileResult::Average(200.0),
            PercentileResult::Value(10.0),
            PercentileResult::Overflow(5000),
            PercentileResult::Average(5.0),
        ];
        xs.sort_by(compare_for_sort);
        assert_eq!(
            xs,
            vec![
                PercentileResult::Value(10.0),
                PercentileResult::Value(50.0),
                PercentileResult::Overflow(5000),
                PercentileResult::Average(5.0),
                PercentileResult::Average(200.0),
                PercentileResult::NoData,
            ]
        );
    }
}
