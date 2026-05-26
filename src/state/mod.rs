//! アプリケーション状態 (App) と派生メトリクス。
//!
//! issue #20 で、`connection.rs` 時代のバナー専用 App から、
//! - `history`: rolling 120 snapshot + sparkline + 再起動検出
//! - `AppStatus`: Connecting / Running / Stale / Disconnected
//! - tab / sort / filter / cursor / detail / error_banner
//!
//! を持つ本番想定の `App` に置き換える。値の埋まり方は段階的:
//! - 本 PR では `on_fetch_ok` / `on_fetch_err` の口だけ作り、UI 系フィールドの
//!   初期値を確定する。
//!
//! 後続 issue:
//! - issue #21-23: `derived` サブモジュールで RPS / BW / percentile を計算
//! - issue #25-: tokio::select! ループ + ratatui 描画
//! - issue #28-30: ソート / フィルタ / 詳細ビュー

use std::time::Instant;

use crate::client::FetchError;
use crate::model::VtsStatus;

pub mod history;

pub use history::{DerivedSnapshot, History, Snapshot, HISTORY_CAPACITY};

/// `Stale` → `Disconnected` に escalate する連続失敗回数の閾値。
///
/// 1 秒間隔で 5 回 = 約 5 秒で「切断」として扱う。issue #25 で interval を CLI
/// 化したら、本値も derive する想定。
pub const DISCONNECTED_THRESHOLD: u32 = 5;

/// テーブルの zone 種別タブ。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    Server,
    Upstream,
    Cache,
}

/// ソート列 / 方向。`column` のセマンティクスはタブごとに異なる (詳細は
/// docs/DESIGN.md)。issue #28 で実装を埋める。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SortState {
    /// 0-based 列 index。デフォルトは 0 (= ZONE 列)。
    pub column: u8,
    /// `true` = 降順。
    pub descending: bool,
}

impl Default for SortState {
    fn default() -> Self {
        Self {
            column: 0,
            descending: true,
        }
    }
}

/// 接続バナーの状態機械。
///
/// 遷移:
/// - 初期: `Connecting`
/// - Connecting + fetch 成功 → `Running`
/// - Connecting + fetch 失敗 → `Disconnected { failures: 1 }` (last_ok 無し)
/// - Running + fetch 失敗 → `Stale { last_ok, failures: 1 }`
/// - Stale + fetch 失敗 → `Stale { failures+=1 }` ただし
///   `DISCONNECTED_THRESHOLD` 到達で `Disconnected { failures }` に escalate
/// - Disconnected + fetch 失敗 → `Disconnected { failures+=1 }`
/// - 任意 + fetch 成功 → `Running` (failures リセット、error_banner クリア)
///
/// UI 設計上の注意:
/// 起動直後 (`Connecting`) の初回失敗は `Disconnected { failures: 1 }` に直接
/// 遷移するが、これは `failures < DISCONNECTED_THRESHOLD` の段階で UI 側が
/// "Connection failed" 等の柔らかい表現に切り替える前提 (issue #25 / #29 で
/// 描画レイヤが実装するときに吸収)。`Disconnected` という名前は内部状態のみで
/// あり、即「切断バナー赤色」を意味しない。
#[derive(Debug, Clone, Default)]
pub enum AppStatus {
    #[default]
    Connecting,
    Running,
    Stale {
        last_ok: Instant,
        failures: u32,
    },
    Disconnected {
        failures: u32,
    },
}

impl AppStatus {
    /// 連続失敗回数。`Connecting` / `Running` は 0。
    pub fn failures(&self) -> u32 {
        match self {
            AppStatus::Connecting | AppStatus::Running => 0,
            AppStatus::Stale { failures, .. } | AppStatus::Disconnected { failures } => *failures,
        }
    }
}

/// vozltop のアプリケーション状態のルート。
///
/// フィールドは UI からも書き換える (cursor, filter, sort, active_tab) 都合上、
/// 全部 `pub` で公開している。値の埋まり方は段階的:
/// - 本 PR (#20) では `history` と `status` を fetch ループから書く。
/// - issue #28 で UI 側が `sort` / `filter` / `cursor` / `detail_zone` を書く。
#[derive(Debug)]
pub struct App {
    /// 接続バナーの状態。
    pub status: AppStatus,
    /// snapshot 履歴 + sparkline + 再起動検出。
    pub history: History,
    /// 現在表示中の zone 種別タブ。
    pub active_tab: Tab,
    /// ソート列 / 方向。
    pub sort: SortState,
    /// zone 名 substring フィルタ (空文字 = 無効)。
    pub filter: String,
    /// 選択中の行 index (フィルタ適用後の表示順での 0-based)。
    pub cursor: usize,
    /// 詳細オーバーレイで表示中の zone 名。`None` で非表示。
    pub detail_zone: Option<String>,
    /// 直近の fetch エラーで表示するバナー文字列。`None` で非表示。
    /// セキュリティ上、URL や認証情報を含まないように
    /// `FetchError::banner_message()` 経由で生成する。
    pub error_banner: Option<String>,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    /// 初期状態を返す。
    pub fn new() -> Self {
        Self {
            status: AppStatus::default(),
            history: History::new(),
            active_tab: Tab::default(),
            sort: SortState::default(),
            filter: String::new(),
            cursor: 0,
            detail_zone: None,
            error_banner: None,
        }
    }

    /// fetch 成功時のハンドラ。
    ///
    /// - `history` に snapshot を push (rolling buffer は自動で 120 件にキャップ)
    /// - `status` を `Running` に
    /// - `error_banner` をクリア
    pub fn on_fetch_ok(&mut self, status: VtsStatus) {
        let snapshot = Snapshot {
            at: Instant::now(),
            status,
            derived: DerivedSnapshot::default(),
        };
        self.history.push(snapshot);
        self.status = AppStatus::Running;
        self.error_banner = None;
    }

    /// fetch 失敗時のハンドラ。
    ///
    /// 状態遷移は `AppStatus` の docstring に従う。バナー文字列は
    /// `FetchError::banner_message()` から生成し、URL / 認証情報を含まない。
    /// 連続失敗中は新規 push が発生しないため、`history.nginx_restart_detected`
    /// が前回 true のまま張り付くのを防ぐべくここでクリアする。
    pub fn on_fetch_err(&mut self, err: &FetchError) {
        self.error_banner = Some(err.banner_message());
        self.history.clear_nginx_restart_flag();
        self.status = match &self.status {
            AppStatus::Connecting => AppStatus::Disconnected { failures: 1 },
            AppStatus::Running => match self.history.last_at() {
                Some(last_ok) => AppStatus::Stale {
                    last_ok,
                    failures: 1,
                },
                // Running なのに history が空 = 通常起こり得ないが、防御的に
                // Disconnected 扱い。
                None => AppStatus::Disconnected { failures: 1 },
            },
            AppStatus::Stale { last_ok, failures } => {
                let next = failures.saturating_add(1);
                if next >= DISCONNECTED_THRESHOLD {
                    AppStatus::Disconnected { failures: next }
                } else {
                    AppStatus::Stale {
                        last_ok: *last_ok,
                        failures: next,
                    }
                }
            }
            AppStatus::Disconnected { failures } => AppStatus::Disconnected {
                failures: failures.saturating_add(1),
            },
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::StatusCode;

    fn ok_status(now_msec: u64) -> VtsStatus {
        let raw = serde_json::json!({
            "hostName": "h", "nginxVersion": "1", "moduleVersion": "v",
            "loadMsec": 0u64, "nowMsec": now_msec,
            "connections": {
                "active": 1, "reading": 0, "writing": 0,
                "waiting": 0, "accepted": 0, "handled": 0, "requests": 0
            },
        });
        serde_json::from_value(raw).unwrap()
    }

    fn http_err() -> FetchError {
        FetchError::Status {
            code: StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    #[test]
    fn new_app_is_connecting() {
        let app = App::new();
        assert!(matches!(app.status, AppStatus::Connecting));
        assert!(app.history.is_empty());
        assert_eq!(app.active_tab, Tab::Server);
        assert_eq!(app.sort.column, 0);
        assert!(app.sort.descending);
        assert!(app.filter.is_empty());
        assert_eq!(app.cursor, 0);
        assert!(app.detail_zone.is_none());
        assert!(app.error_banner.is_none());
    }

    #[test]
    fn on_fetch_ok_pushes_history_and_clears_banner() {
        let mut app = App::new();
        app.error_banner = Some("old banner".to_string());

        app.on_fetch_ok(ok_status(1000));

        assert!(matches!(app.status, AppStatus::Running));
        assert_eq!(app.history.len(), 1);
        assert!(app.error_banner.is_none());
    }

    #[test]
    fn connecting_to_disconnected_on_first_failure() {
        let mut app = App::new();
        app.on_fetch_err(&http_err());

        match app.status {
            AppStatus::Disconnected { failures } => assert_eq!(failures, 1),
            other => panic!("expected Disconnected, got {other:?}"),
        }
        assert_eq!(
            app.error_banner.as_deref(),
            Some("HTTP 500 Internal Server Error")
        );
    }

    #[test]
    fn running_then_failure_goes_to_stale() {
        let mut app = App::new();
        app.on_fetch_ok(ok_status(1000));
        app.on_fetch_err(&http_err());

        match app.status {
            AppStatus::Stale { failures, .. } => assert_eq!(failures, 1),
            other => panic!("expected Stale, got {other:?}"),
        }
    }

    #[test]
    fn stale_escalates_to_disconnected_at_threshold() {
        let mut app = App::new();
        app.on_fetch_ok(ok_status(1000));
        for _ in 0..(DISCONNECTED_THRESHOLD - 1) {
            app.on_fetch_err(&http_err());
        }
        // ここまでで failures = THRESHOLD - 1 < THRESHOLD なので Stale のまま
        assert!(matches!(app.status, AppStatus::Stale { .. }));

        app.on_fetch_err(&http_err());
        // ここで failures = THRESHOLD なので Disconnected に escalate
        match app.status {
            AppStatus::Disconnected { failures } => {
                assert_eq!(failures, DISCONNECTED_THRESHOLD)
            }
            other => panic!("expected Disconnected, got {other:?}"),
        }
    }

    #[test]
    fn disconnected_failures_count_up() {
        let mut app = App::new();
        app.on_fetch_err(&http_err()); // 1
        app.on_fetch_err(&http_err()); // 2
        app.on_fetch_err(&http_err()); // 3

        match app.status {
            AppStatus::Disconnected { failures } => assert_eq!(failures, 3),
            other => panic!("expected Disconnected, got {other:?}"),
        }
    }

    #[test]
    fn success_after_failure_resets_to_running() {
        let mut app = App::new();
        app.on_fetch_err(&http_err());
        app.on_fetch_err(&http_err());
        assert!(matches!(app.status, AppStatus::Disconnected { .. }));
        assert!(app.error_banner.is_some());

        app.on_fetch_ok(ok_status(1000));

        assert!(matches!(app.status, AppStatus::Running));
        assert!(app.error_banner.is_none());
        assert_eq!(app.history.len(), 1);
    }

    #[test]
    fn appstatus_failures_helper() {
        assert_eq!(AppStatus::Connecting.failures(), 0);
        assert_eq!(AppStatus::Running.failures(), 0);
        assert_eq!(
            AppStatus::Stale {
                last_ok: Instant::now(),
                failures: 3
            }
            .failures(),
            3
        );
        assert_eq!(AppStatus::Disconnected { failures: 7 }.failures(), 7);
    }

    #[test]
    fn default_app_matches_new() {
        let a = App::default();
        let b = App::new();
        assert!(matches!(a.status, AppStatus::Connecting));
        assert_eq!(a.active_tab, b.active_tab);
    }

    #[test]
    fn nginx_restart_propagates_through_history() {
        let mut app = App::new();
        app.on_fetch_ok(ok_status(1_000_000));
        assert!(!app.history.nginx_restart_detected());
        app.on_fetch_ok(ok_status(500));
        assert!(app.history.nginx_restart_detected());
    }

    #[test]
    fn on_fetch_err_clears_nginx_restart_flag() {
        // PR #61 レビュー指摘: fetch 連続失敗中に restart flag が張り付くのを防ぐ。
        let mut app = App::new();
        app.on_fetch_ok(ok_status(1_000_000));
        app.on_fetch_ok(ok_status(500)); // restart 検知
        assert!(app.history.nginx_restart_detected());

        app.on_fetch_err(&http_err());
        assert!(
            !app.history.nginx_restart_detected(),
            "fetch 失敗時に restart flag をクリアすること"
        );
    }
}
