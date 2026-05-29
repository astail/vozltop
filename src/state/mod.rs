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

use std::cell::Cell;
use std::time::Instant;

use crate::cli::Args;
use crate::client::FetchError;
use crate::model::VtsStatus;
use crate::theme::Theme;

pub mod derived;
pub mod history;
pub mod percentile;

pub use derived::{
    compute, CacheDerived, DerivedSnapshot, ServerDerived, StatusRatios, UpstreamDerived, ZoneRates,
};
pub use history::{History, Snapshot, HISTORY_CAPACITY};
pub use percentile::{average_fallback, compare_for_sort, percentile, PercentileResult};

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

/// アラート閾値 (issue #47)。`--alert-5xx-pct` / `--alert-p95-ms` から確定する。
///
/// いずれかの閾値以上の指標を持つ行を `ui::table` がハイライトし、新たにアラート
/// 行が出現した瞬間に `main.rs` が端末ベルを 1 度鳴らす。閾値未設定 (`None`) の
/// 指標は判定に寄与しない。TOML 設定との統合は issue #46 に委ねる。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct AlertConfig {
    /// 5xx 率 (%) の上限。これ以上でアラート。
    pub max_5xx_pct: Option<f64>,
    /// p95 レイテンシ (ms) の上限。これ以上でアラート。
    pub max_p95_ms: Option<u64>,
}

impl AlertConfig {
    /// `Args` から確定する (theme と同じ `from_args` 規約)。
    pub fn from_args(args: &Args) -> Self {
        Self {
            max_5xx_pct: args.alert_5xx_pct,
            max_p95_ms: args.alert_p95_ms,
        }
    }

    /// いずれかの閾値が設定されていれば `true` (= アラート判定を行う)。
    pub fn is_enabled(&self) -> bool {
        self.max_5xx_pct.is_some() || self.max_p95_ms.is_some()
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
/// - issue #26 で `theme` を追加 (NO_COLOR / --no-color 連動)。
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
    /// UI 配色 (`Theme::color()` / `Theme::mono()`)。
    ///
    /// `main.rs` 起動時に `Theme::from_args(&args)` で確定する。テストや
    /// `App::new()` 経由では `Theme::default()` (= color) が入る。
    pub theme: Theme,
    /// 直近の table render で確定した「現在タブの可視行数」(cursor 上限算出用)。
    ///
    /// `ui::table::render` が描画のたびに更新する。cursor 移動メソッド
    /// (`cursor_down` / `cursor_page_down`) はこの値を上限としてクランプする。
    /// 描画前 (= snapshot 取得前) は 0。
    ///
    /// `Cell<usize>` で interior mutability にしているのは、`ui::render` が
    /// `&App` を取る既存契約を壊さず、レンダパスから値だけ書き戻すため
    /// (issue #28 で導入)。
    pub visible_rows: Cell<usize>,
    /// 直近の table render で確定した「1 PgUp / PgDn ぶんの移動量」。
    ///
    /// `ui::table::render` が描画領域の本体高さに合わせて毎フレーム更新する。
    /// 描画前は安全側で 10 行。
    pub page_size: Cell<usize>,
    /// F1 / `?` で開閉する help overlay の表示有無 (issue #33)。
    ///
    /// `true` で `src/ui/help.rs` がモーダルを画面中央に重ねる。`Esc` で
    /// 閉じる。詳細オーバーレイ (`detail_zone`) や filter / sort 状態とは
    /// 独立に扱う (互いを排他しない)。
    pub show_help: bool,
    /// アラート閾値 (issue #47)。`main.rs` 起動時に `AlertConfig::from_args` で
    /// 確定する。`App::new()` / `with_theme` 経由では無効 (`Default` = 全 `None`)。
    pub alerts: AlertConfig,
    /// 直近の fetch 時点で「アラート行が 1 つ以上あったか」。ベルを rising edge
    /// (false→true) でのみ鳴らすための状態。`update_alert_active` が更新する。
    pub alert_active: bool,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    /// 初期状態を返す (テーマは `Theme::default()` = color)。
    pub fn new() -> Self {
        Self::with_theme(Theme::default())
    }

    /// 任意のテーマで初期化する。`main.rs` から `Theme::from_args(&args)` を
    /// 渡して呼ぶことを想定。
    pub fn with_theme(theme: Theme) -> Self {
        Self {
            status: AppStatus::default(),
            history: History::new(),
            active_tab: Tab::default(),
            sort: SortState::default(),
            filter: String::new(),
            cursor: 0,
            detail_zone: None,
            error_banner: None,
            theme,
            visible_rows: Cell::new(0),
            // 安全側のデフォルト。最初の render が走るまで PgUp/PgDn が完全に
            // no-op にならないよう、画面の半分弱に相当する 10 行を仮置きする。
            page_size: Cell::new(10),
            show_help: false,
            alerts: AlertConfig::default(),
            alert_active: false,
        }
    }

    /// `↑` / `k` 相当の cursor 移動。0 を下限に saturating で減らす。
    pub fn cursor_up(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// `↓` / `j` 相当の cursor 移動。`visible_rows - 1` を上限にクランプ。
    pub fn cursor_down(&mut self) {
        let max = self.visible_rows.get().saturating_sub(1);
        if self.cursor < max {
            self.cursor += 1;
        }
    }

    /// `PgUp` 相当。`page_size` ぶん saturating で減らす。
    pub fn cursor_page_up(&mut self) {
        let step = self.page_size.get().max(1);
        self.cursor = self.cursor.saturating_sub(step);
    }

    /// `PgDn` 相当。`page_size` ぶん進めて `visible_rows - 1` を上限にクランプ。
    pub fn cursor_page_down(&mut self) {
        let step = self.page_size.get().max(1);
        let max = self.visible_rows.get().saturating_sub(1);
        self.cursor = self.cursor.saturating_add(step).min(max);
    }

    /// アラート状態を更新し、rising edge (`false`→`true`) なら `true` を返す。
    ///
    /// `main.rs` が fetch 成功ごとに「現在アラート行があるか」を渡して呼ぶ。
    /// 戻り値が `true` のときだけ端末ベルを鳴らすことで、アラートが継続している
    /// 間に毎秒鳴り続けるのを防ぐ (アラートが一度解消して再発したら再び鳴る)。
    pub fn update_alert_active(&mut self, alerting: bool) -> bool {
        let rising = alerting && !self.alert_active;
        self.alert_active = alerting;
        rising
    }

    /// バナー描画用の文字列を返す。mono 時のみ `[!] ` プレフィックスを付与する
    /// (issue #26 受け入れ条件)。
    ///
    /// UI レイヤ (issue #27 以降) はこの戻り値を `Paragraph` 等に流すだけで、
    /// プレフィックス分岐を自前で持たなくてよい。
    pub fn error_banner_display(&self) -> Option<String> {
        self.error_banner
            .as_deref()
            .map(|msg| format!("{}{msg}", self.theme.error_banner_prefix()))
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
        // 既定テーマは color。
        assert!(!app.theme.mono);
        // cursor 移動上限算出用 (issue #28)。初期は 0 行 / 10 行ぶんの「ページ」。
        assert_eq!(app.visible_rows.get(), 0);
        assert_eq!(app.page_size.get(), 10);
    }

    // ---------- cursor 移動 (issue #28) ----------

    #[test]
    fn cursor_up_saturates_at_zero() {
        let mut app = App::new();
        app.visible_rows.set(5);
        app.cursor_up();
        assert_eq!(app.cursor, 0);
        app.cursor = 3;
        app.cursor_up();
        assert_eq!(app.cursor, 2);
    }

    #[test]
    fn cursor_down_clamps_to_visible_rows_minus_one() {
        let mut app = App::new();
        app.visible_rows.set(3);
        for _ in 0..10 {
            app.cursor_down();
        }
        assert_eq!(app.cursor, 2, "max = visible_rows - 1");
    }

    #[test]
    fn cursor_down_with_no_rows_stays_at_zero() {
        let mut app = App::new();
        // visible_rows = 0 (default)
        app.cursor_down();
        assert_eq!(app.cursor, 0);
    }

    #[test]
    fn cursor_page_up_subtracts_page_size_saturating() {
        let mut app = App::new();
        app.visible_rows.set(100);
        app.page_size.set(10);
        app.cursor = 7;
        app.cursor_page_up();
        assert_eq!(app.cursor, 0);
        app.cursor = 25;
        app.cursor_page_up();
        assert_eq!(app.cursor, 15);
    }

    #[test]
    fn cursor_page_down_advances_and_clamps() {
        let mut app = App::new();
        app.visible_rows.set(50);
        app.page_size.set(10);
        app.cursor_page_down();
        assert_eq!(app.cursor, 10);
        app.cursor = 45;
        app.cursor_page_down();
        assert_eq!(app.cursor, 49, "clamp to visible_rows - 1");
    }

    #[test]
    fn cursor_page_methods_treat_zero_page_size_as_one() {
        let mut app = App::new();
        app.visible_rows.set(10);
        app.page_size.set(0);
        app.cursor_page_down();
        assert_eq!(app.cursor, 1);
        app.cursor_page_up();
        assert_eq!(app.cursor, 0);
    }

    #[test]
    fn with_theme_stores_theme() {
        let app = App::with_theme(Theme::mono());
        assert!(app.theme.mono);
        // 他フィールドは new() と同じ初期値
        assert!(matches!(app.status, AppStatus::Connecting));
        assert!(app.history.is_empty());
    }

    #[test]
    fn error_banner_display_returns_none_when_no_banner() {
        let app = App::new();
        assert!(app.error_banner_display().is_none());
    }

    #[test]
    fn error_banner_display_has_no_prefix_in_color_mode() {
        let mut app = App::with_theme(Theme::color());
        app.error_banner = Some("HTTP 500 Internal Server Error".to_string());
        assert_eq!(
            app.error_banner_display().as_deref(),
            Some("HTTP 500 Internal Server Error")
        );
    }

    #[test]
    fn error_banner_display_has_bang_prefix_in_mono_mode() {
        let mut app = App::with_theme(Theme::mono());
        app.error_banner = Some("HTTP 500 Internal Server Error".to_string());
        assert_eq!(
            app.error_banner_display().as_deref(),
            Some("[!] HTTP 500 Internal Server Error")
        );
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

    // ---------- issue #47: アラート閾値 ----------

    #[test]
    fn alert_config_is_enabled_only_when_a_threshold_is_set() {
        assert!(!AlertConfig::default().is_enabled());
        assert!(AlertConfig {
            max_5xx_pct: Some(1.0),
            max_p95_ms: None,
        }
        .is_enabled());
        assert!(AlertConfig {
            max_5xx_pct: None,
            max_p95_ms: Some(500),
        }
        .is_enabled());
    }

    #[test]
    fn new_app_has_alerts_disabled() {
        let app = App::new();
        assert!(!app.alerts.is_enabled());
        assert!(!app.alert_active);
    }

    #[test]
    fn update_alert_active_rings_only_on_rising_edge() {
        let mut app = App::new();
        // false → true: rising edge なので鳴らす
        assert!(app.update_alert_active(true));
        // true → true: 継続中は鳴らさない
        assert!(!app.update_alert_active(true));
        // true → false: 解消 (鳴らさない)
        assert!(!app.update_alert_active(false));
        // false → true: 再発で再び鳴らす
        assert!(app.update_alert_active(true));
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
