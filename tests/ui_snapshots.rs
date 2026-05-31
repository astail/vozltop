//! UI レンダリング全体の insta snapshot テスト (issue #35)。
//!
//! `ratatui::backend::TestBackend` で 80×24 の画面を描画し、その Display 出力
//! (= バッファのシンボルを枠付きで文字列化したもの) を insta snapshot に焼き付ける。
//! TestBackend の Display は色/スタイルを含まずシンボルのみなので、theme (color/mono)
//! に依存せず決定的。レンダ経路に経過時間表示は無く、入力 (固定 fixture) が同じなら
//! 出力も常に同じになる。
//!
//! ## カバレッジ
//!
//! - 4 状態 (`Connecting` / `Running` / `Stale` / `Disconnected`) × 4 タブ
//!   (`Server` / `Upstream` / `Cache` / `Filter`) = 16 枚
//! - 詳細オーバーレイ (Server zone) 1 枚
//! - help モーダル 1 枚
//!
//! ## snapshot の更新手順
//!
//! 描画を意図的に変えたときは `cargo insta review` で差分を確認して accept する
//! (詳細は CONTRIBUTING.md)。`*.snap.new` を残したままマージはできない。

use insta::assert_snapshot;
use ratatui::backend::TestBackend;
use ratatui::Terminal;

use vozltop::model::VtsStatus;
use vozltop::state::{App, AppStatus, Tab, Workspace};

const W: u16 = 80;
const H: u16 = 24;

/// 連続した 2 snapshot の prev (実 nginx-vts 応答)。
fn initial() -> VtsStatus {
    serde_json::from_str(include_str!("fixtures/initial.json")).expect("initial.json parse")
}

/// 連続した 2 snapshot の now (initial から ~16.7s 後にトラフィックが乗った状態)。
fn after() -> VtsStatus {
    serde_json::from_str(include_str!("fixtures/after_traffic.json"))
        .expect("after_traffic.json parse")
}

/// initial → after_traffic を push した「Running」App。2 snapshot あるので
/// RPS / BW sparkline と派生メトリクス (RPS / p95 / ratio) が描画される。
fn running_app() -> App {
    let mut app = App::new();
    app.on_fetch_ok(initial());
    app.on_fetch_ok(after());
    app
}

/// `app` を 80×24 で描画し、TestBackend の Display 文字列を返す。
fn render_to_string(app: &App) -> String {
    let backend = TestBackend::new(W, H);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal
        .draw(|f| vozltop::ui::render(f, app))
        .expect("draw");
    format!("{}", terminal.backend())
}

/// 3 タブそれぞれに `app` を切り替えて snapshot を撮る。
/// `mk` は毎回新しい App を返すクロージャ (状態を共有しないため)。
fn snapshot_each_tab(prefix: &str, mk: impl Fn() -> App) {
    for (tab, suffix) in [
        (Tab::Server, "server"),
        (Tab::Upstream, "upstream"),
        (Tab::Cache, "cache"),
        (Tab::Filter, "filter"),
    ] {
        let mut app = mk();
        app.active_tab = tab;
        assert_snapshot!(format!("{prefix}_{suffix}"), render_to_string(&app));
    }
}

// ---------- 4 状態 × 3 タブ ----------

#[test]
fn snapshot_connecting_tabs() {
    // snapshot 未取得。各タブは "waiting for first VTS snapshot…" プレースホルダ。
    snapshot_each_tab("connecting", App::new);
}

#[test]
fn snapshot_running_tabs() {
    snapshot_each_tab("running", running_app);
}

#[test]
fn snapshot_stale_tabs() {
    // 直近データは保持したまま fetch が失敗し始めた状態。
    snapshot_each_tab("stale", || {
        let mut app = running_app();
        app.status = AppStatus::Stale {
            last_ok: std::time::Instant::now(),
            failures: 2,
        };
        app.error_banner = Some("HTTP 500 Internal Server Error".to_string());
        app
    });
}

#[test]
fn snapshot_disconnected_tabs() {
    snapshot_each_tab("disconnected", || {
        let mut app = running_app();
        app.status = AppStatus::Disconnected { failures: 5 };
        app.error_banner = Some("HTTP 500 Internal Server Error".to_string());
        app
    });
}

// ---------- オーバーレイ ----------

#[test]
fn snapshot_detail_overlay() {
    // Server zone の詳細オーバーレイ (p50/p95/p99 + histogram)。
    let mut app = running_app();
    app.active_tab = Tab::Server;
    app.detail_zone = Some("api.example.test".to_string());
    assert_snapshot!("detail_overlay_server", render_to_string(&app));
}

#[test]
fn snapshot_help_overlay() {
    let mut app = running_app();
    app.show_help = true;
    assert_snapshot!("help_overlay", render_to_string(&app));
}

// ---------- multi-host (issue #44) ----------

/// Workspace を 80×24 で描画し、TestBackend の Display 文字列を返す。
fn render_workspace_to_string(ws: &Workspace) -> String {
    let backend = TestBackend::new(W, H);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal
        .draw(|f| vozltop::ui::render_workspace(f, ws))
        .expect("draw");
    format!("{}", terminal.backend())
}

#[test]
fn snapshot_multi_host_running_server() {
    // 3 host の Workspace で Server タブを描画。最上段に Host タブバーが入る。
    let ws = Workspace::new(vec![
        ("web-prod-1".to_string(), running_app()),
        ("web-prod-2".to_string(), running_app()),
        ("edge-tokyo".to_string(), running_app()),
    ]);
    assert_snapshot!("multi_host_running_server", render_workspace_to_string(&ws));
}

#[test]
fn snapshot_multi_host_single_is_identical_to_render() {
    // 単一 host の Workspace は host タブバーを描画せず、`render(f, app)` と
    // 同一の出力になる (= 既存 snapshot と差分が出ない)。
    let ws = Workspace::single_host(running_app());
    let multi_out = render_workspace_to_string(&ws);
    let plain_out = render_to_string(&running_app());
    assert_eq!(
        multi_out, plain_out,
        "single-host Workspace は render(f, app) と完全同一の出力"
    );
}
