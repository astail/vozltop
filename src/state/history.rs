//! 直近 120 件の `VtsStatus` snapshot を rolling buffer で保持する `History`、
//! および sliding-window peak / 累計値 helper。
//!
//! issue #20 で型と push の口を整備し、issue #21 で `DerivedSnapshot` の実体を
//! `state::derived` に切り出した。issue #150 でヘッダの sparkline / Gauge を
//! 廃止し、`rps_history` / `bw_*_history` / `rolling_max_active_conns` /
//! `push_derived` を全削除。代わりに 60s sliding-window peak (`peak_rps` /
//! `peak_bw`) と累計値 (`total_*` / `uptime_ms`) を提供する。

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::model::VtsStatus;
use crate::state::derived::DerivedSnapshot;

/// rolling buffer の最大長。1 秒間隔で 120 サンプル ≒ 2 分の履歴。
pub const HISTORY_CAPACITY: usize = 120;

/// peak 計算の wall-clock window 長 (issue #150)。
///
/// `--interval ≥ 0.5s` で 60s 全体を `HISTORY_CAPACITY (=120)` 内に収められる。
/// 極端に短い interval (< 0.5s) では window 末尾が clip されるが、interval を
/// 変えても peak の追従挙動が wall-clock 60s で一定であることを優先する。
pub const PEAK_WINDOW: Duration = Duration::from_secs(60);

/// `peak_rps` の最低値 (= 無負荷時に bar が 100% に張り付くのを防ぐ floor)。
const RPS_FLOOR: u64 = 1;
/// `peak_bw` の最低値 (= 同上、bytes/s)。
const BW_FLOOR: u64 = 1024;

/// 1 tick ぶんの観測値。
#[derive(Debug, Clone)]
pub struct Snapshot {
    /// snapshot を受信した時刻 (クライアント側時計)。
    pub at: Instant,
    /// nginx-vts から取得した生の状態。
    pub status: VtsStatus,
    /// 派生メトリクス (RPS / BW/s / percentile)。
    pub derived: DerivedSnapshot,
}

/// `Snapshot` の rolling buffer + 派生集計用 helper。
///
/// issue #150 で旧 sparkline 履歴 (`rps_history` / `bw_*_history`) と
/// `rolling_max_active_conns` を撤去。peak 系は `snapshots` 上を毎回走査して
/// 算出する (60 sample / call、ほぼ無視できるコスト)。
#[derive(Debug, Default)]
pub struct History {
    snapshots: VecDeque<Snapshot>,
    /// 直前の `push` で nginx 再起動 (`nowMsec` の単調性違反) を検出したか。
    /// `compute` が None を返す判定と同じ。次の push で自動更新される transient flag。
    nginx_restart_detected: bool,
}

impl History {
    /// 空の History を返す。
    pub fn new() -> Self {
        Self::default()
    }

    /// snapshot を末尾に追加する。
    ///
    /// - `snapshots.len()` が `HISTORY_CAPACITY` を超えると先頭を pop する。
    /// - `nginx_restart_detected` を、直前 snapshot との `nowMsec` 比較で更新。
    pub fn push(&mut self, snapshot: Snapshot) {
        // nginx 再起動検出は append 前の latest と比較する
        self.nginx_restart_detected = match self.snapshots.back() {
            Some(prev) => snapshot.status.now_msec < prev.status.now_msec,
            None => false,
        };

        self.snapshots.push_back(snapshot);
        if self.snapshots.len() > HISTORY_CAPACITY {
            self.snapshots.pop_front();
        }
    }

    /// 保存されている snapshot 数 (最大 `HISTORY_CAPACITY`)。
    pub fn len(&self) -> usize {
        self.snapshots.len()
    }

    /// `snapshots` が空か。
    pub fn is_empty(&self) -> bool {
        self.snapshots.is_empty()
    }

    /// 最新の `Snapshot` への借用。空なら `None`。
    pub fn latest(&self) -> Option<&Snapshot> {
        self.snapshots.back()
    }

    /// 最新 snapshot の取得時刻 (`Instant`)。`AppStatus::Stale.last_ok` 用。
    pub fn last_at(&self) -> Option<Instant> {
        self.snapshots.back().map(|s| s.at)
    }

    /// 2 つ手前の snapshot への借用 (派生メトリクスの diff 計算用)。
    pub fn previous(&self) -> Option<&Snapshot> {
        let len = self.snapshots.len();
        if len < 2 {
            None
        } else {
            self.snapshots.get(len - 2)
        }
    }

    /// 直前 push で nginx 再起動 (`nowMsec` 単調性違反) を検出したか。
    pub fn nginx_restart_detected(&self) -> bool {
        self.nginx_restart_detected
    }

    /// `nginx_restart_detected` を強制的に false に戻す。
    ///
    /// fetch 失敗中は push されないので、flag を立てたまま「再起動バナー」が
    /// 長時間張り付くのを防ぐため、`App::on_fetch_err` が呼ぶ。
    pub fn clear_nginx_restart_flag(&mut self) {
        self.nginx_restart_detected = false;
    }

    // ---------- issue #150: sliding-window peak ----------

    /// 直近 `PEAK_WINDOW` (60s) の RPS peak。floor = `RPS_FLOOR` (=1)。
    ///
    /// 走査基点は `latest.at` (= fetch 失敗中に古い peak が伸び続けないため、
    /// `Instant::now()` 基点ではなく)。snapshot ごとに
    /// `s.derived.server_totals().0` を見る。
    pub fn peak_rps(&self) -> u64 {
        self.window_max(|s| s.derived.server_totals().0, RPS_FLOOR)
    }

    /// 直近 `PEAK_WINDOW` の bandwidth peak (bytes/s)。floor = `BW_FLOOR` (=1024)。
    ///
    /// IN/OUT 共通の分母として使う。per-sample で `max(bw_in, bw_out)` を取り
    /// その上で window 内の max を返す (数学的に
    /// `max(max_t(in_t), max_t(out_t)) == max_t(max(in_t, out_t))`)。
    pub fn peak_bw(&self) -> u64 {
        self.window_max(
            |s| {
                let (_, i, o) = s.derived.server_totals();
                i.max(o)
            },
            BW_FLOOR,
        )
    }

    /// 共通 helper: latest から `PEAK_WINDOW` 内の sample に対して `pick` を適用
    /// した最大値を返す。空 history は `floor`。
    fn window_max<F>(&self, pick: F, floor: u64) -> u64
    where
        F: Fn(&Snapshot) -> u64,
    {
        let Some(latest_at) = self.last_at() else {
            return floor;
        };
        let cutoff = latest_at.checked_sub(PEAK_WINDOW);
        let mut best = 0u64;
        for s in self.snapshots.iter().rev() {
            if let Some(c) = cutoff {
                if s.at < c {
                    break;
                }
            }
            let v = pick(s);
            if v > best {
                best = v;
            }
        }
        best.max(floor)
    }

    // ---------- issue #150: 累計値 helper ----------

    /// 累計リクエスト数。`connections.requests` (nginx コアの累積) を使う。
    ///
    /// 取り出し元の選択理由: zone 設定有無に依存せず nginx が捌いた総数を出す
    /// のが運用上「累計」として読みやすい。server_zones[*] 集計と同じ値には
    /// ならない (handshake / 4xx without zone を含むため) ことに注意。
    /// 空 history は 0。
    pub fn total_requests(&self) -> u64 {
        self.latest()
            .map(|s| s.status.connections.requests)
            .unwrap_or(0)
    }

    /// 累計受信バイト数 (server_zones 合算)。`*` zone があればそれを使う
    /// (issue #117 と同じ二重計上回避)。空 history は 0。
    pub fn total_in_bytes(&self) -> u64 {
        let Some(latest) = self.latest() else {
            return 0;
        };
        if let Some(star) = latest.status.server_zones.get("*") {
            return star.in_bytes;
        }
        latest
            .status
            .server_zones
            .values()
            .map(|z| z.in_bytes)
            .sum()
    }

    /// 累計送信バイト数 (server_zones 合算)。`*` zone preference は
    /// [`History::total_in_bytes`] と同じ。空 history は 0。
    pub fn total_out_bytes(&self) -> u64 {
        let Some(latest) = self.latest() else {
            return 0;
        };
        if let Some(star) = latest.status.server_zones.get("*") {
            return star.out_bytes;
        }
        latest
            .status
            .server_zones
            .values()
            .map(|z| z.out_bytes)
            .sum()
    }

    /// nginx の起動からの経過時間 (ミリ秒)。`now_msec - load_msec`。
    /// 空 history は 0。
    pub fn uptime_ms(&self) -> u64 {
        let Some(latest) = self.latest() else {
            return 0;
        };
        latest
            .status
            .now_msec
            .saturating_sub(latest.status.load_msec)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::derived::{ServerDerived, StatusRatios, ZoneRates};

    fn snapshot(now_msec: u64, active_conns: u64) -> Snapshot {
        let raw = serde_json::json!({
            "hostName": "h", "nginxVersion": "1", "moduleVersion": "v",
            "loadMsec": 0u64, "nowMsec": now_msec,
            "connections": {
                "active": active_conns, "reading": 0, "writing": 0,
                "waiting": 0, "accepted": 0, "handled": 0, "requests": 0
            },
        });
        let status: VtsStatus = serde_json::from_value(raw).unwrap();
        Snapshot {
            at: Instant::now(),
            status,
            derived: DerivedSnapshot::default(),
        }
    }

    /// 任意の `at` / RPS / IN / OUT を持つ Snapshot を作る (peak テスト用)。
    fn snapshot_at(at: Instant, rps: u64, bw_in: u64, bw_out: u64) -> Snapshot {
        let raw = serde_json::json!({
            "hostName": "h", "nginxVersion": "1", "moduleVersion": "v",
            "loadMsec": 0u64, "nowMsec": 0u64,
            "connections": {
                "active": 0, "reading": 0, "writing": 0,
                "waiting": 0, "accepted": 0, "handled": 0, "requests": 0
            },
        });
        let status: VtsStatus = serde_json::from_value(raw).unwrap();
        let mut derived = DerivedSnapshot::default();
        // server_totals() が `*` を優先するので、`*` zone に値を直接入れる
        derived.server.insert(
            "*".to_string(),
            ServerDerived {
                rates: ZoneRates {
                    rps: rps as f64,
                    bw_in_per_sec: bw_in as f64,
                    bw_out_per_sec: bw_out as f64,
                },
                ratios: StatusRatios::default(),
            },
        );
        Snapshot {
            at,
            status,
            derived,
        }
    }

    #[test]
    fn new_history_is_empty() {
        let h = History::new();
        assert!(h.is_empty());
        assert_eq!(h.len(), 0);
        assert!(h.latest().is_none());
        assert!(h.previous().is_none());
        assert!(!h.nginx_restart_detected());
    }

    #[test]
    fn push_grows_until_capacity() {
        let mut h = History::new();
        for i in 0..HISTORY_CAPACITY {
            h.push(snapshot(1000 + i as u64, 0));
        }
        assert_eq!(h.len(), HISTORY_CAPACITY);
    }

    #[test]
    fn push_beyond_capacity_drops_oldest() {
        let mut h = History::new();
        for i in 0..HISTORY_CAPACITY + 1 {
            h.push(snapshot(1000 + i as u64, 0));
        }
        assert_eq!(h.len(), HISTORY_CAPACITY);
        assert_eq!(
            h.snapshots.front().unwrap().status.now_msec,
            1001,
            "oldest entry should be popped"
        );
        assert_eq!(h.latest().unwrap().status.now_msec, 1120);
    }

    #[test]
    fn nginx_restart_flag_fires_on_nowmsec_regression() {
        let mut h = History::new();
        h.push(snapshot(1_000_000, 0));
        assert!(!h.nginx_restart_detected());

        h.push(snapshot(500, 0));
        assert!(h.nginx_restart_detected());

        h.push(snapshot(1500, 0));
        assert!(!h.nginx_restart_detected());
    }

    #[test]
    fn equal_nowmsec_is_not_treated_as_restart() {
        let mut h = History::new();
        h.push(snapshot(1_000_000, 5));
        h.push(snapshot(1_000_000, 7));
        assert!(
            !h.nginx_restart_detected(),
            "同値 nowMsec は restart と見做さない"
        );
    }

    #[test]
    fn clear_nginx_restart_flag_resets_to_false() {
        let mut h = History::new();
        h.push(snapshot(1_000_000, 0));
        h.push(snapshot(500, 0));
        assert!(h.nginx_restart_detected());
        h.clear_nginx_restart_flag();
        assert!(!h.nginx_restart_detected());
    }

    #[test]
    fn latest_and_previous_track_order() {
        let mut h = History::new();
        assert!(h.latest().is_none());
        h.push(snapshot(1000, 0));
        assert_eq!(h.latest().unwrap().status.now_msec, 1000);
        assert!(h.previous().is_none());

        h.push(snapshot(1001, 0));
        assert_eq!(h.latest().unwrap().status.now_msec, 1001);
        assert_eq!(h.previous().unwrap().status.now_msec, 1000);
    }

    #[test]
    fn last_at_returns_instant_of_latest_snapshot() {
        let mut h = History::new();
        assert!(h.last_at().is_none());
        h.push(snapshot(1000, 0));
        assert!(h.last_at().is_some());
    }

    // ---------- issue #150: peak ----------

    #[test]
    fn peak_returns_floor_when_history_empty() {
        let h = History::new();
        assert_eq!(h.peak_rps(), RPS_FLOOR);
        assert_eq!(h.peak_bw(), BW_FLOOR);
    }

    #[test]
    fn peak_returns_floor_when_all_samples_below_floor() {
        let mut h = History::new();
        let t0 = Instant::now();
        h.push(snapshot_at(t0, 0, 0, 0));
        assert_eq!(h.peak_rps(), RPS_FLOOR, "floor when all samples are 0");
        assert_eq!(h.peak_bw(), BW_FLOOR);
    }

    #[test]
    fn peak_rps_excludes_samples_older_than_window() {
        let mut h = History::new();
        let now = Instant::now();
        // 70s 前の大きい値 (window 外)
        h.push(snapshot_at(now - Duration::from_secs(70), 5000, 0, 0));
        // 30s 前の中程度 (window 内)
        h.push(snapshot_at(now - Duration::from_secs(30), 800, 0, 0));
        // 直近 (window 内)
        h.push(snapshot_at(now, 200, 0, 0));
        assert_eq!(h.peak_rps(), 800, "older than 60s must be excluded");
    }

    #[test]
    fn peak_bw_uses_per_sample_max_of_in_and_out() {
        let mut h = History::new();
        let now = Instant::now();
        // sample1: IN=1000, OUT=5000 → per-sample max = 5000
        h.push(snapshot_at(now - Duration::from_secs(10), 0, 1000, 5000));
        // sample2: IN=6000, OUT=2000 → per-sample max = 6000
        h.push(snapshot_at(now, 0, 6000, 2000));
        // 値が BW_FLOOR (1024) より十分大きいので floor で潰れない
        assert_eq!(h.peak_bw(), 6000, "per-sample max(in, out) で集計");
    }

    #[test]
    fn peak_unaffected_by_nginx_restart_marker() {
        // restart 前後の sample がどちらも max 候補になること (peak は
        // nowMsec の単調性ではなく Snapshot.at の wall-clock で判定するため)。
        let mut h = History::new();
        let now = Instant::now();
        h.push(snapshot_at(now - Duration::from_secs(10), 1000, 0, 0));
        h.push(snapshot_at(now, 300, 0, 0));
        assert_eq!(h.peak_rps(), 1000);
    }

    // ---------- issue #150: 累計 ----------

    #[test]
    fn total_requests_uses_connections_requests() {
        let mut h = History::new();
        let raw = serde_json::json!({
            "hostName": "h", "nginxVersion": "1", "moduleVersion": "v",
            "loadMsec": 0u64, "nowMsec": 0u64,
            "connections": {
                "active": 0, "reading": 0, "writing": 0,
                "waiting": 0, "accepted": 0, "handled": 0, "requests": 12345
            },
        });
        let status: VtsStatus = serde_json::from_value(raw).unwrap();
        h.push(Snapshot {
            at: Instant::now(),
            status,
            derived: DerivedSnapshot::default(),
        });
        assert_eq!(h.total_requests(), 12345);
    }

    #[test]
    fn total_in_out_bytes_prefer_star_zone_when_present() {
        // issue #117 と同じ二重計上回避: `*` zone があればそれだけを使う
        let mut h = History::new();
        let raw = serde_json::json!({
            "hostName": "h", "nginxVersion": "1", "moduleVersion": "v",
            "loadMsec": 0u64, "nowMsec": 0u64,
            "connections": {
                "active": 0, "reading": 0, "writing": 0,
                "waiting": 0, "accepted": 0, "handled": 0, "requests": 0
            },
            "serverZones": {
                "api": {
                    "requestCounter": 0, "inBytes": 1000, "outBytes": 10000,
                    "responses": {"1xx":0,"2xx":0,"3xx":0,"4xx":0,"5xx":0,
                        "miss":0,"bypass":0,"expired":0,"stale":0,
                        "updating":0,"revalidated":0,"hit":0,"scarce":0},
                    "requestMsec": 0, "requestMsecCounter": 0,
                    "requestBuckets": {"msecs": [], "counters": []}
                },
                "www": {
                    "requestCounter": 0, "inBytes": 500, "outBytes": 5000,
                    "responses": {"1xx":0,"2xx":0,"3xx":0,"4xx":0,"5xx":0,
                        "miss":0,"bypass":0,"expired":0,"stale":0,
                        "updating":0,"revalidated":0,"hit":0,"scarce":0},
                    "requestMsec": 0, "requestMsecCounter": 0,
                    "requestBuckets": {"msecs": [], "counters": []}
                },
                "*": {
                    "requestCounter": 0, "inBytes": 1500, "outBytes": 15000,
                    "responses": {"1xx":0,"2xx":0,"3xx":0,"4xx":0,"5xx":0,
                        "miss":0,"bypass":0,"expired":0,"stale":0,
                        "updating":0,"revalidated":0,"hit":0,"scarce":0},
                    "requestMsec": 0, "requestMsecCounter": 0,
                    "requestBuckets": {"msecs": [], "counters": []}
                }
            }
        });
        let status: VtsStatus = serde_json::from_value(raw).unwrap();
        h.push(Snapshot {
            at: Instant::now(),
            status,
            derived: DerivedSnapshot::default(),
        });
        assert_eq!(
            h.total_in_bytes(),
            1500,
            "`*` zone preference (二重計上回避)"
        );
        assert_eq!(h.total_out_bytes(), 15000);
    }

    #[test]
    fn total_in_out_bytes_fall_back_to_sum_when_star_absent() {
        let mut h = History::new();
        let raw = serde_json::json!({
            "hostName": "h", "nginxVersion": "1", "moduleVersion": "v",
            "loadMsec": 0u64, "nowMsec": 0u64,
            "connections": {
                "active": 0, "reading": 0, "writing": 0,
                "waiting": 0, "accepted": 0, "handled": 0, "requests": 0
            },
            "serverZones": {
                "api": {
                    "requestCounter": 0, "inBytes": 1000, "outBytes": 10000,
                    "responses": {"1xx":0,"2xx":0,"3xx":0,"4xx":0,"5xx":0,
                        "miss":0,"bypass":0,"expired":0,"stale":0,
                        "updating":0,"revalidated":0,"hit":0,"scarce":0},
                    "requestMsec": 0, "requestMsecCounter": 0,
                    "requestBuckets": {"msecs": [], "counters": []}
                },
                "www": {
                    "requestCounter": 0, "inBytes": 500, "outBytes": 5000,
                    "responses": {"1xx":0,"2xx":0,"3xx":0,"4xx":0,"5xx":0,
                        "miss":0,"bypass":0,"expired":0,"stale":0,
                        "updating":0,"revalidated":0,"hit":0,"scarce":0},
                    "requestMsec": 0, "requestMsecCounter": 0,
                    "requestBuckets": {"msecs": [], "counters": []}
                }
            }
        });
        let status: VtsStatus = serde_json::from_value(raw).unwrap();
        h.push(Snapshot {
            at: Instant::now(),
            status,
            derived: DerivedSnapshot::default(),
        });
        assert_eq!(h.total_in_bytes(), 1500);
        assert_eq!(h.total_out_bytes(), 15000);
    }

    #[test]
    fn uptime_ms_is_now_minus_load() {
        let mut h = History::new();
        let raw = serde_json::json!({
            "hostName": "h", "nginxVersion": "1", "moduleVersion": "v",
            "loadMsec": 1_000_000u64, "nowMsec": 1_300_000u64,
            "connections": {
                "active": 0, "reading": 0, "writing": 0,
                "waiting": 0, "accepted": 0, "handled": 0, "requests": 0
            },
        });
        let status: VtsStatus = serde_json::from_value(raw).unwrap();
        h.push(Snapshot {
            at: Instant::now(),
            status,
            derived: DerivedSnapshot::default(),
        });
        assert_eq!(h.uptime_ms(), 300_000);
    }

    #[test]
    fn uptime_ms_zero_when_empty() {
        let h = History::new();
        assert_eq!(h.uptime_ms(), 0);
    }
}
