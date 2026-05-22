//! fetch 結果を受けて connection banner と nginx 再起動検出を更新する `App`。
//!
//! 設計ポイント:
//! - 連続失敗カウンタは `1 〜 STALE_LIMIT-1` で `Stale data`、それ以上で
//!   `Disconnected` バナーに切り替える。issue #19 仕様で 5 連続失敗を境に。
//! - 成功した瞬間に counter を 0 へリセットし `Running` に戻す。
//! - nginx 再起動 (`nowMsec` が前回より小さい場合) を検出したら
//!   `restart_detected = true` を 1 tick 立てる。次の派生メトリクス計算 (issue
//!   #21〜#23) はこのフラグを見て当該 tick の派生値計算を skip し、prev を
//!   更新するのみに留める。

use crate::client::FetchError;
use crate::model::VtsStatus;

/// `consecutive_failures` がこの値 **以上** で `Disconnected` バナーに切り替える。
/// `1..LIMIT` の範囲は `Stale data` を表示する。issue #19 仕様で 5。
pub const DISCONNECTED_THRESHOLD: u32 = 5;

/// バナー / ステータスバーに表示する接続状態。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum BannerStatus {
    /// 直近の fetch が成功した状態。
    #[default]
    Running,
    /// 1〜4 連続失敗。前回の成功スナップショット (`App::latest`) はまだ表示する
    /// 価値がある (古いが直近の値) ため "Stale data" を出す。
    Stale {
        consecutive_failures: u32,
        reason: String,
    },
    /// 5 回以上連続失敗。古すぎる値を信用しないようバナーを切り替える。
    Disconnected {
        consecutive_failures: u32,
        reason: String,
    },
}

impl BannerStatus {
    /// 表示用の単一行文字列 ("Running" / "Stale data: HTTP 503 ..." / ...)。
    pub fn label(&self) -> String {
        match self {
            BannerStatus::Running => "Running".to_string(),
            BannerStatus::Stale {
                consecutive_failures,
                reason,
            } => format!("Stale data ({consecutive_failures}/{DISCONNECTED_THRESHOLD}): {reason}"),
            BannerStatus::Disconnected {
                consecutive_failures,
                reason,
            } => format!("Disconnected ({consecutive_failures} consecutive failures): {reason}"),
        }
    }
}

/// fetch 結果を受け付けるアプリケーション状態。
///
/// issue #20 で `history: VecDeque<VtsStatus>` (rolling 120) に拡張される。
/// 本 PR では `latest` のみ保持する。
#[derive(Debug, Default)]
pub struct App {
    /// 直近の成功 fetch で得た `VtsStatus`。fetch 失敗中も Stale 表示用に保持
    /// しておき、`Disconnected` に切り替わってからは UI 側がフェードアウト等を
    /// 判断する想定。
    pub latest: Option<VtsStatus>,
    /// 連続失敗カウンタ。成功時に 0 へリセット。
    pub consecutive_failures: u32,
    /// 現在のバナー状態。
    pub banner: BannerStatus,
    /// 直前の fetch で nginx 再起動 (`nowMsec` の単調性違反) を検出したか。
    /// この tick 限定の transient フラグ。次の `on_fetch_ok` / `on_fetch_err`
    /// 呼び出しで自動的に false に戻る。
    pub restart_detected: bool,
}

impl App {
    pub fn new() -> Self {
        Self {
            latest: None,
            consecutive_failures: 0,
            banner: BannerStatus::Running,
            restart_detected: false,
        }
    }

    /// fetch が成功した場合の状態遷移。
    ///
    /// - `nowMsec` が前回より小さい場合は nginx 再起動と判定し
    ///   `restart_detected = true`。派生メトリクスは prev を更新するだけで
    ///   計算しない方針 (issue #21〜#23 で実装)。
    /// - 連続失敗カウンタを 0 へリセット、バナーを `Running` に。
    pub fn on_fetch_ok(&mut self, snapshot: VtsStatus) {
        let restart = match &self.latest {
            Some(prev) => snapshot.now_msec < prev.now_msec,
            None => false,
        };
        self.restart_detected = restart;
        self.latest = Some(snapshot);
        self.consecutive_failures = 0;
        self.banner = BannerStatus::Running;
    }

    /// fetch が失敗した場合の状態遷移。
    ///
    /// 連続失敗カウンタを `saturating_add(1)` で増やし、`DISCONNECTED_THRESHOLD`
    /// 未満なら `Stale`、以上なら `Disconnected`。`reason` は
    /// `FetchError::banner_message()` 由来で URL や secret を含まない。
    pub fn on_fetch_err(&mut self, err: &FetchError) {
        self.restart_detected = false;
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        let reason = err.banner_message();
        self.banner = if self.consecutive_failures >= DISCONNECTED_THRESHOLD {
            BannerStatus::Disconnected {
                consecutive_failures: self.consecutive_failures,
                reason,
            }
        } else {
            BannerStatus::Stale {
                consecutive_failures: self.consecutive_failures,
                reason,
            }
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(now_msec: u64) -> VtsStatus {
        // 最小有効 VtsStatus を組み立てる。serde_json::from_value のほうが
        // 短く書けるためそちらを使う。
        let raw = serde_json::json!({
            "hostName": "test", "nginxVersion": "1.27.3", "moduleVersion": "v0.2.5",
            "loadMsec": 0u64, "nowMsec": now_msec,
            "connections": {"active":0,"reading":0,"writing":0,"waiting":0,"accepted":0,"handled":0,"requests":0},
        });
        serde_json::from_value(raw).unwrap()
    }

    #[test]
    fn new_app_starts_running_with_zero_failures() {
        let app = App::new();
        assert_eq!(app.consecutive_failures, 0);
        assert_eq!(app.banner, BannerStatus::Running);
        assert!(app.latest.is_none());
        assert!(!app.restart_detected);
    }

    #[test]
    fn first_failure_transitions_to_stale() {
        let mut app = App::new();
        app.on_fetch_err(&FetchError::Timeout);
        assert_eq!(app.consecutive_failures, 1);
        assert!(matches!(app.banner, BannerStatus::Stale { .. }));
    }

    #[test]
    fn fourth_failure_still_stale() {
        let mut app = App::new();
        for _ in 0..4 {
            app.on_fetch_err(&FetchError::Timeout);
        }
        assert_eq!(app.consecutive_failures, 4);
        assert!(matches!(app.banner, BannerStatus::Stale { .. }));
    }

    #[test]
    fn fifth_failure_transitions_to_disconnected() {
        let mut app = App::new();
        for _ in 0..5 {
            app.on_fetch_err(&FetchError::Timeout);
        }
        assert_eq!(app.consecutive_failures, 5);
        assert!(matches!(app.banner, BannerStatus::Disconnected { .. }));
    }

    #[test]
    fn success_resets_counter_and_banner() {
        let mut app = App::new();
        for _ in 0..5 {
            app.on_fetch_err(&FetchError::Timeout);
        }
        assert!(matches!(app.banner, BannerStatus::Disconnected { .. }));

        app.on_fetch_ok(snapshot(1000));
        assert_eq!(app.consecutive_failures, 0);
        assert_eq!(app.banner, BannerStatus::Running);
        assert!(app.latest.is_some());
    }

    #[test]
    fn nginx_restart_sets_restart_detected_flag() {
        let mut app = App::new();
        app.on_fetch_ok(snapshot(1_000_000));
        assert!(!app.restart_detected, "first ok should not flag restart");

        app.on_fetch_ok(snapshot(500)); // nowMsec が大幅に減少
        assert!(
            app.restart_detected,
            "nowMsec regression should flag restart"
        );
        assert_eq!(app.consecutive_failures, 0);
    }

    #[test]
    fn restart_detected_resets_on_next_success_without_regression() {
        let mut app = App::new();
        app.on_fetch_ok(snapshot(1_000_000));
        app.on_fetch_ok(snapshot(500));
        assert!(app.restart_detected);

        // 通常進行
        app.on_fetch_ok(snapshot(1500));
        assert!(
            !app.restart_detected,
            "restart flag should be transient and cleared on next ok"
        );
    }

    #[test]
    fn restart_detected_resets_on_failure() {
        let mut app = App::new();
        app.on_fetch_ok(snapshot(1_000_000));
        app.on_fetch_ok(snapshot(500));
        assert!(app.restart_detected);

        app.on_fetch_err(&FetchError::Timeout);
        assert!(!app.restart_detected);
    }

    #[test]
    fn banner_label_does_not_include_url_or_secret() {
        // FetchError::banner_message() 側で URL を含まないことを担保。
        // ここでは reason が "HTTP 503" のような形になっていることを確認。
        let mut app = App::new();
        app.on_fetch_err(&FetchError::Status {
            code: reqwest::StatusCode::SERVICE_UNAVAILABLE,
        });
        let label = app.banner.label();
        assert!(
            label.contains("503"),
            "label should include status: {label}"
        );
        assert!(
            !label.to_lowercase().contains("http://"),
            "label must not include URL: {label}"
        );
        assert!(
            !label.to_lowercase().contains("password"),
            "label must not include 'password': {label}"
        );
    }

    #[test]
    fn saturating_counter_does_not_overflow() {
        let mut app = App {
            latest: None,
            consecutive_failures: u32::MAX - 1,
            banner: BannerStatus::Running,
            restart_detected: false,
        };
        app.on_fetch_err(&FetchError::Timeout);
        app.on_fetch_err(&FetchError::Timeout);
        // saturating_add で MAX に張り付くだけで panic しない
        assert_eq!(app.consecutive_failures, u32::MAX);
    }
}
