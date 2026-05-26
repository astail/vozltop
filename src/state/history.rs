//! 直近 120 件の `VtsStatus` snapshot を rolling buffer で保持する `History`、
//! および sparkline / Gauge auto-scale 用の集計値。
//!
//! issue #20 では型と push の口だけ作る。`DerivedSnapshot` の実体は issue
//! #21〜#23 で `state::derived` モジュールに分離される予定で、本 PR では
//! 空 struct の placeholder。

use std::collections::VecDeque;
use std::time::Instant;

use crate::model::VtsStatus;

/// rolling buffer の最大長。1 秒間隔で 120 サンプル ≒ 2 分の履歴。
pub const HISTORY_CAPACITY: usize = 120;

/// 1 tick ぶんの観測値。
#[derive(Debug, Clone)]
pub struct Snapshot {
    /// snapshot を受信した時刻 (クライアント側時計)。
    pub at: Instant,
    /// nginx-vts から取得した生の状態。
    pub status: VtsStatus,
    /// 派生メトリクス (RPS / BW/s / percentile)。
    /// issue #21〜#23 で実装、本 PR では空 placeholder。
    pub derived: DerivedSnapshot,
}

/// 派生メトリクス placeholder。
///
/// issue #21〜#23 で以下を埋める想定:
/// - `rps: u64` / `bw_in_per_sec: u64` / `bw_out_per_sec: u64` (zone 横断集計)
/// - `server_p95: HashMap<String, Option<u64>>` 等の per-zone percentile
///
/// 本 PR では空にすることで「型は存在するが値は無い」状態にし、derive(Default)
/// で App / History が初期化できることを担保する。
#[derive(Debug, Clone, Default)]
pub struct DerivedSnapshot {
    /// TODO(issue #21): RPS / BW/s
    /// TODO(issue #22): p50/p95/p99 per zone
    #[allow(dead_code)]
    _placeholder: (),
}

/// `Snapshot` の rolling buffer + sparkline 用集計 + Gauge auto-scale 用 rolling
/// max。
///
/// - `snapshots`: 最大 `HISTORY_CAPACITY` 件。121 件目を push すると最古が落ちる。
/// - `rps_history` / `bw_in_history` / `bw_out_history`: zone 横断の集計値の
///   時系列。issue #21 で `push_derived` 相当のメソッドが値を埋めるまでは空。
/// - `rolling_max_active_conns`: 観測中の最大 `connections.active`。
///   `connections.worker_connections` が VTS JSON に存在しないため、Gauge の
///   auto-scale 用に使う (CLAUDE.md 設計判断より)。
#[derive(Debug, Default)]
pub struct History {
    snapshots: VecDeque<Snapshot>,
    rps_history: VecDeque<u64>,
    bw_in_history: VecDeque<u64>,
    bw_out_history: VecDeque<u64>,
    rolling_max_active_conns: u64,
    /// 直前の `push` で nginx 再起動 (`nowMsec` の単調性違反) を検出したか。
    /// issue #21〜#23 の派生メトリクス計算は、本フラグが立っている tick の
    /// 派生値計算を skip し、prev を更新するだけにする方針。
    /// 次の push で自動的に更新される transient フラグ。
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
    /// - `rolling_max_active_conns` を更新。
    /// - `nginx_restart_detected` を、直前 snapshot との `nowMsec` 比較で更新。
    pub fn push(&mut self, snapshot: Snapshot) {
        // nginx 再起動検出は append 前の latest と比較する
        self.nginx_restart_detected = match self.snapshots.back() {
            Some(prev) => snapshot.status.now_msec < prev.status.now_msec,
            None => false,
        };

        let active = snapshot.status.connections.active;
        self.rolling_max_active_conns = self.rolling_max_active_conns.max(active);

        self.snapshots.push_back(snapshot);
        if self.snapshots.len() > HISTORY_CAPACITY {
            self.snapshots.pop_front();
        }
    }

    /// 1 tick ぶんの zone 横断派生値 (RPS / BW/s) を sparkline 履歴に追加する。
    ///
    /// issue #21 の RPS / BW 差分計算ロジックがこの口を叩く。本 PR ではテストでのみ
    /// 直接呼ぶ。`HISTORY_CAPACITY` を超えると古いものから落ちる。
    pub fn push_derived(&mut self, rps: u64, bw_in_per_sec: u64, bw_out_per_sec: u64) {
        push_capped(&mut self.rps_history, rps);
        push_capped(&mut self.bw_in_history, bw_in_per_sec);
        push_capped(&mut self.bw_out_history, bw_out_per_sec);
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

    /// 最新 snapshot の取得時刻 (`Instant`)。issue #20 の `AppStatus::Stale.last_ok`
    /// に渡す用途で使う。
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

    /// 観測中の `connections.active` の最大値 (Gauge の auto-scale 用)。
    pub fn rolling_max_active_conns(&self) -> u64 {
        self.rolling_max_active_conns
    }

    /// 直前 push で nginx 再起動 (`nowMsec` 単調性違反) を検出したか。
    pub fn nginx_restart_detected(&self) -> bool {
        self.nginx_restart_detected
    }

    /// `nginx_restart_detected` を強制的に false に戻す。
    ///
    /// fetch 失敗中は push されないので、flag を立てたまま「再起動バナー」が
    /// 長時間張り付くのを防ぐため、`App::on_fetch_err` が呼ぶ想定。
    pub fn clear_nginx_restart_flag(&mut self) {
        self.nginx_restart_detected = false;
    }

    /// 集計値 sparkline 履歴 (RPS) への借用。
    pub fn rps_history(&self) -> &VecDeque<u64> {
        &self.rps_history
    }

    /// 集計値 sparkline 履歴 (in BW/s) への借用。
    pub fn bw_in_history(&self) -> &VecDeque<u64> {
        &self.bw_in_history
    }

    /// 集計値 sparkline 履歴 (out BW/s) への借用。
    pub fn bw_out_history(&self) -> &VecDeque<u64> {
        &self.bw_out_history
    }
}

fn push_capped(deque: &mut VecDeque<u64>, value: u64) {
    deque.push_back(value);
    if deque.len() > HISTORY_CAPACITY {
        deque.pop_front();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn new_history_is_empty() {
        let h = History::new();
        assert!(h.is_empty());
        assert_eq!(h.len(), 0);
        assert!(h.latest().is_none());
        assert!(h.previous().is_none());
        assert_eq!(h.rolling_max_active_conns(), 0);
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
        // 121 件 push して 1 件落ちることを確認 (受け入れ条件)
        for i in 0..HISTORY_CAPACITY + 1 {
            h.push(snapshot(1000 + i as u64, 0));
        }
        assert_eq!(h.len(), HISTORY_CAPACITY);
        // 最古は now_msec = 1001 (= index 1)。0 が落ちた。
        assert_eq!(
            h.snapshots.front().unwrap().status.now_msec,
            1001,
            "oldest entry should be popped"
        );
        // 最新は now_msec = 1000 + 120 = 1120
        assert_eq!(h.latest().unwrap().status.now_msec, 1120);
    }

    #[test]
    fn rolling_max_active_conns_tracks_max() {
        let mut h = History::new();
        h.push(snapshot(1000, 5));
        h.push(snapshot(1001, 30));
        h.push(snapshot(1002, 10));
        h.push(snapshot(1003, 50));
        h.push(snapshot(1004, 20));
        assert_eq!(h.rolling_max_active_conns(), 50);

        // 50 を超える値が来ない限り上書きはされない (rolling-max は falling-edge を追わない)
        h.push(snapshot(1005, 1));
        assert_eq!(h.rolling_max_active_conns(), 50);
    }

    #[test]
    fn nginx_restart_flag_fires_on_nowmsec_regression() {
        let mut h = History::new();
        h.push(snapshot(1_000_000, 0));
        assert!(!h.nginx_restart_detected());

        h.push(snapshot(500, 0)); // nowMsec が大幅に減少
        assert!(h.nginx_restart_detected());

        h.push(snapshot(1500, 0)); // 通常進行
        assert!(!h.nginx_restart_detected());
    }

    #[test]
    fn equal_nowmsec_is_not_treated_as_restart() {
        // 受信間隔が短くて時計分解能が足りない場合、同じ nowMsec が連続することは
        // ありうる。strict less ではなく `<=` で判定していると false positive で
        // 「再起動」と表示してしまうため、回帰防止に固定する。
        let mut h = History::new();
        h.push(snapshot(1_000_000, 5));
        h.push(snapshot(1_000_000, 7));
        assert!(
            !h.nginx_restart_detected(),
            "同値 nowMsec は restart と見做さない"
        );
        assert_eq!(h.rolling_max_active_conns(), 7);
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
        assert!(h.previous().is_none()); // 1 件しかない

        h.push(snapshot(1001, 0));
        assert_eq!(h.latest().unwrap().status.now_msec, 1001);
        assert_eq!(h.previous().unwrap().status.now_msec, 1000);

        h.push(snapshot(1002, 0));
        assert_eq!(h.latest().unwrap().status.now_msec, 1002);
        assert_eq!(h.previous().unwrap().status.now_msec, 1001);
    }

    #[test]
    fn push_derived_caps_each_sparkline() {
        let mut h = History::new();
        for i in 0..HISTORY_CAPACITY + 10 {
            h.push_derived(i as u64, (i * 2) as u64, (i * 3) as u64);
        }
        assert_eq!(h.rps_history().len(), HISTORY_CAPACITY);
        assert_eq!(h.bw_in_history().len(), HISTORY_CAPACITY);
        assert_eq!(h.bw_out_history().len(), HISTORY_CAPACITY);
        // 最古 10 件が drop されているので front は index = 10
        assert_eq!(*h.rps_history().front().unwrap(), 10);
        assert_eq!(*h.bw_in_history().front().unwrap(), 20);
        assert_eq!(*h.bw_out_history().front().unwrap(), 30);
    }

    #[test]
    fn last_at_returns_instant_of_latest_snapshot() {
        let mut h = History::new();
        assert!(h.last_at().is_none());
        h.push(snapshot(1000, 0));
        assert!(h.last_at().is_some());
    }
}
