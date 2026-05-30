//! zone 名フィルタ (substring + 大文字小文字無視) を end-to-end でロックする
//! (issue #37)。
//!
//! `src/state/mod.rs::tests` の `push_filter_char` 等は `App::filter` 文字列の
//! 編集だけを検証する。本ファイルは「`filter` が rendered table の行に対して
//! どう効くか」、つまり実行時の **マッチ規則** (= 部分一致 + 大文字小文字無視)
//! を render 経由で確認する。
//!
//! render を経由するのは、`ui::table::retain_matching` が `pub(crate)` で外から
//! 直接呼べないため。`App::visible_rows` を読めば render の確定後の表示行数が
//! 取れる (issue #28 で `Cell<usize>` として公開)。

use ratatui::backend::TestBackend;
use ratatui::Terminal;

use vozltop::model::VtsStatus;
use vozltop::state::{App, Tab};

const W: u16 = 100;
const H: u16 = 24;

/// テスト用に 3 zone の serverZones を持つ最小の `VtsStatus` を返す。
///
/// zone 名は大文字小文字の組み合わせ・別位置に同じ部分文字列を含むように選び、
/// 「位置を問わない部分一致」「大小無視」の双方を 1 fixture で検証できるよう
/// にしてある。
///
/// - `AlphaApi`     — 'A' で始まる
/// - `BravoZone`    — 中央〜末尾に `Zone` (大文字 Z) を含む
/// - `charlieTest`  — 末尾に `Test` (大文字 T) を含む / 全体は小文字始まり
fn status_with_zones() -> VtsStatus {
    let raw = serde_json::json!({
        "hostName": "h",
        "nginxVersion": "1",
        "moduleVersion": "v",
        "loadMsec": 0u64,
        "nowMsec": 1_000u64,
        "connections": {
            "active": 0, "reading": 0, "writing": 0,
            "waiting": 0, "accepted": 0, "handled": 0, "requests": 0
        },
        "serverZones": {
            "AlphaApi":     server_zone_obj(),
            "BravoZone":    server_zone_obj(),
            "charlieTest":  server_zone_obj(),
        }
    });
    serde_json::from_value(raw).expect("status_with_zones must parse")
}

/// `ServerZone` の必須フィールドを全部 0 で埋めた JSON オブジェクトを返す。
fn server_zone_obj() -> serde_json::Value {
    serde_json::json!({
        "requestCounter": 0, "inBytes": 0, "outBytes": 0,
        "responses": {
            "1xx": 0, "2xx": 0, "3xx": 0, "4xx": 0, "5xx": 0,
            "miss": 0, "bypass": 0, "expired": 0, "stale": 0,
            "updating": 0, "revalidated": 0, "hit": 0, "scarce": 0
        },
        "requestMsec": 0, "requestMsecCounter": 0,
        "requestBuckets": { "msecs": [], "counters": [] }
    })
}

/// `app` を W×H で 1 度描画し、確定した `visible_rows` と TestBackend の
/// Display 文字列を返す。
fn render_and_snapshot(app: &App) -> (usize, String) {
    let backend = TestBackend::new(W, H);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal
        .draw(|f| vozltop::ui::render(f, app))
        .expect("draw");
    let text = format!("{}", terminal.backend());
    (app.visible_rows.get(), text)
}

/// 3 zone snapshot を push 済みの Server タブ App を返す。
///
/// `App::with_theme(Theme::mono())` は使わない — Theme は filter 挙動に影響しないが、
/// render で色を吐かない方が出力文字列の安定性 (snapshot grep) が上がる。とはいえ
/// TestBackend の Display は元々シンボルのみ (色情報を捨てる) なので、ここでは
/// default theme のままで構わない。
fn app_with_three_zones() -> App {
    let mut app = App::new();
    app.on_fetch_ok(status_with_zones());
    app.active_tab = Tab::Server;
    app
}

// ---------- 空フィルタは絞り込み無し ----------

#[test]
fn empty_filter_keeps_all_rows() {
    let app = app_with_three_zones();
    let (rows, text) = render_and_snapshot(&app);

    assert_eq!(rows, 3, "all 3 zones visible when filter is empty");
    assert!(text.contains("AlphaApi"), "AlphaApi in output");
    assert!(text.contains("BravoZone"), "BravoZone in output");
    assert!(text.contains("charlieTest"), "charlieTest in output");
}

// ---------- 大文字小文字無視 (filter 側が小文字) ----------

#[test]
fn lowercase_filter_matches_mixed_case_zone() {
    // filter = "bravo" (小文字) が "BravoZone" (B が大文字) にマッチ
    let mut app = app_with_three_zones();
    app.filter = "bravo".to_string();
    let (rows, text) = render_and_snapshot(&app);

    assert_eq!(rows, 1, "only BravoZone matches");
    assert!(text.contains("BravoZone"));
    assert!(!text.contains("AlphaApi"));
    assert!(!text.contains("charlieTest"));
}

// ---------- 大文字小文字無視 (filter 側が大文字) ----------

#[test]
fn uppercase_filter_matches_lowercase_zone() {
    // filter = "TEST" (大文字) が "charlieTest" (T だけ大文字) にマッチ。
    // "charlieTest" は末尾の "Test" を含むので、これだけがヒットする。
    let mut app = app_with_three_zones();
    app.filter = "TEST".to_string();
    let (rows, text) = render_and_snapshot(&app);

    assert_eq!(rows, 1, "only charlieTest matches TEST (case-insensitive)");
    assert!(text.contains("charlieTest"));
    assert!(!text.contains("AlphaApi"));
    assert!(!text.contains("BravoZone"));
}

// ---------- 部分一致 (位置を問わない) ----------

#[test]
fn substring_matches_at_any_position() {
    // "zone" は "BravoZone" の中央〜末尾にあり、prefix でなくマッチする
    let mut app = app_with_three_zones();
    app.filter = "zone".to_string();
    let (rows, text) = render_and_snapshot(&app);

    assert_eq!(rows, 1);
    assert!(text.contains("BravoZone"));
}

#[test]
fn single_char_substring_matches_any_zone_containing_it() {
    // "z" は "BravoZone" にのみ含まれる
    let mut app = app_with_three_zones();
    app.filter = "z".to_string();
    let (rows, text) = render_and_snapshot(&app);

    assert_eq!(rows, 1, "only BravoZone has a 'z'");
    assert!(text.contains("BravoZone"));
    assert!(!text.contains("AlphaApi"));
    assert!(!text.contains("charlieTest"));
}

#[test]
fn shared_substring_matches_multiple_zones() {
    // "a" は AlphaApi (Alph**a**) と BravoZone (Br**a**vo) と charlieTest
    // (ch**a**rlie) のすべてに含まれる → 3 行残る
    let mut app = app_with_three_zones();
    app.filter = "a".to_string();
    let (rows, _text) = render_and_snapshot(&app);

    assert_eq!(rows, 3, "'a' is in all three zone names");
}

// ---------- マッチ無し ----------

#[test]
fn no_match_yields_zero_rows() {
    let mut app = app_with_three_zones();
    app.filter = "nonexistent".to_string();
    let (rows, text) = render_and_snapshot(&app);

    assert_eq!(rows, 0, "no zone matches 'nonexistent'");
    assert!(!text.contains("AlphaApi"));
    assert!(!text.contains("BravoZone"));
    assert!(!text.contains("charlieTest"));
}

// ---------- フィルタ編集 API の挙動が render と一致する ----------

#[test]
fn push_filter_char_narrows_visible_rows_progressively() {
    // 1 文字ずつ push して visible_rows が単調減少することを確認する。
    // 編集 API (push_filter_char) と render パスが同じ `filter` を見ていること
    // のロック。
    let mut app = app_with_three_zones();
    let (rows_all, _) = render_and_snapshot(&app);
    assert_eq!(rows_all, 3);

    app.enter_filter();
    app.push_filter_char('b');
    let (rows_b, _) = render_and_snapshot(&app);
    // "b" は BravoZone のみ (Alpha/charlie に b は含まれない)
    assert_eq!(rows_b, 1);

    app.pop_filter_char();
    let (rows_after_pop, _) = render_and_snapshot(&app);
    assert_eq!(rows_after_pop, 3, "pop restores all rows");

    app.clear_filter();
    assert!(!app.filter_active);
    assert!(app.filter.is_empty());
    let (rows_cleared, _) = render_and_snapshot(&app);
    assert_eq!(rows_cleared, 3);
}
