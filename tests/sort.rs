//! 数字キー (1-9) → ソート列マッピングを全タブで end-to-end でロックする
//! (issue #37)。
//!
//! `src/state/sort.rs` / `src/state/mod.rs` の inline テストは個別関数
//! (`column_at` / `apply_sort_key`) を直接呼ぶ単体テスト。本ファイルは
//! **公開 API 越し** (`App::apply_sort_key` → `sort.column` → `column_at`) の
//! 表面契約を docs/DESIGN.md「ソート列マッピング」と照合する。
//!
//! key は CLAUDE.md / DESIGN.md 表記に合わせて 1-based (キーボード「1」キー =
//! 1)。`apply_sort_key` 内で `key - 1` の 0-based 列 index に変換される。

use vozltop::state::sort::{column_at, SortColumn};
use vozltop::state::{App, SortState, Tab};

fn app_on_tab(tab: Tab) -> App {
    let mut app = App::new();
    app.active_tab = tab;
    app
}

/// `(tab, key, expected_column)` の表に対し `apply_sort_key` 経由で
/// `sort.column` が期待 index に動き、その index が期待 `SortColumn` を指すか
/// を一括で検証する。
fn assert_key_maps(tab: Tab, cases: &[(u8, SortColumn)]) {
    for &(key, expected) in cases {
        let mut app = app_on_tab(tab);
        app.apply_sort_key(key);
        let idx = app.sort.column;
        assert_eq!(
            idx,
            key - 1,
            "{tab:?}: key {key} → 0-based index {} (= key - 1)",
            key - 1
        );
        assert_eq!(
            column_at(tab, idx),
            Some(expected),
            "{tab:?} key {key} (column index {idx}) should map to {expected:?}",
        );
    }
}

// ---------- Server タブ: 1-8 ----------

#[test]
fn server_keys_1_through_8_map_to_design_columns() {
    // docs/DESIGN.md「ソート列マッピング」Server タブ:
    // 1=ZONE 2=RPS 3=2xx% 4=4xx% 5=5xx% 6=p95 7=IN/s 8=OUT/s
    assert_key_maps(
        Tab::Server,
        &[
            (1, SortColumn::Zone),
            (2, SortColumn::Rps),
            (3, SortColumn::R2xx),
            (4, SortColumn::R4xx),
            (5, SortColumn::R5xx),
            (6, SortColumn::P95),
            (7, SortColumn::InPerSec),
            (8, SortColumn::OutPerSec),
        ],
    );
}

#[test]
fn server_key_9_is_noop() {
    // Server タブには 9 列目 (STATE) が無いので key 9 は no-op
    let mut app = app_on_tab(Tab::Server);
    let before = app.sort;
    app.apply_sort_key(9);
    assert_eq!(app.sort, before, "out-of-range key on Server is no-op");
}

// ---------- Upstream タブ: 1-9 ----------

#[test]
fn upstream_keys_1_through_9_map_to_design_columns() {
    // Upstream タブは Server + STATE (= 9 列)
    assert_key_maps(
        Tab::Upstream,
        &[
            (1, SortColumn::Zone),
            (2, SortColumn::Rps),
            (3, SortColumn::R2xx),
            (4, SortColumn::R4xx),
            (5, SortColumn::R5xx),
            (6, SortColumn::P95),
            (7, SortColumn::InPerSec),
            (8, SortColumn::OutPerSec),
            (9, SortColumn::State),
        ],
    );
}

// ---------- Cache タブ: 1-8 ----------

#[test]
fn cache_keys_1_through_8_map_to_design_columns() {
    // docs/DESIGN.md Cache タブ:
    // 1=ZONE 2=HIT% 3=MISS 4=EXPIRED 5=STALE 6=USED 7=IN/s 8=OUT/s
    assert_key_maps(
        Tab::Cache,
        &[
            (1, SortColumn::Zone),
            (2, SortColumn::HitPct),
            (3, SortColumn::Miss),
            (4, SortColumn::Expired),
            (5, SortColumn::Stale),
            (6, SortColumn::Used),
            (7, SortColumn::InPerSec),
            (8, SortColumn::OutPerSec),
        ],
    );
}

#[test]
fn cache_key_9_is_noop() {
    let mut app = app_on_tab(Tab::Cache);
    let before = app.sort;
    app.apply_sort_key(9);
    assert_eq!(app.sort, before, "Cache tab has only 8 columns");
}

// ---------- Filter タブ: 1-8 (Server と同形) ----------

#[test]
fn filter_keys_1_through_8_match_server_layout() {
    // Filter タブは filterZones の各 key が serverZones と同形 (group/key)
    // のため、数字キーマッピングは Server と完全に一致する。
    assert_key_maps(
        Tab::Filter,
        &[
            (1, SortColumn::Zone),
            (2, SortColumn::Rps),
            (3, SortColumn::R2xx),
            (4, SortColumn::R4xx),
            (5, SortColumn::R5xx),
            (6, SortColumn::P95),
            (7, SortColumn::InPerSec),
            (8, SortColumn::OutPerSec),
        ],
    );
}

#[test]
fn filter_key_9_is_noop() {
    let mut app = app_on_tab(Tab::Filter);
    let before = app.sort;
    app.apply_sort_key(9);
    assert_eq!(app.sort, before, "Filter tab has only 8 columns");
}

// ---------- key 0 / 範囲外の無視 ----------

#[test]
fn key_zero_is_noop_on_all_tabs() {
    for tab in [Tab::Server, Tab::Upstream, Tab::Cache, Tab::Filter] {
        let mut app = app_on_tab(tab);
        let before = app.sort;
        app.apply_sort_key(0);
        assert_eq!(app.sort, before, "{tab:?}: key 0 is no-op");
    }
}

// ---------- 列切替時の振る舞い ----------

#[test]
fn switching_column_resets_direction_to_descending_and_cursor_to_zero() {
    // 別列に切り替えると並びが変わって元の選択行が別物になるため、cursor は
    // 先頭に戻る。方向は新列の自然順 (= descending) にリセット。
    let mut app = app_on_tab(Tab::Server);
    app.sort = SortState {
        column: 5, // p95
        descending: false,
    };
    app.cursor = 7;

    app.apply_sort_key(2); // key 2 = RPS (column 1)

    assert_eq!(app.sort.column, 1);
    assert!(app.sort.descending, "new column resets to descending");
    assert_eq!(app.cursor, 0, "switching column resets cursor");
}

#[test]
fn pressing_same_column_key_toggles_direction_only() {
    // 同じ列を再指定したときは方向だけが反転 (F5 と同じ挙動)。
    // cursor は維持する (並びの向きが変わるだけで行集合は同じ)。
    let mut app = app_on_tab(Tab::Server);
    app.apply_sort_key(3); // 2xx% を選択 (desc)
    app.cursor = 4;

    app.apply_sort_key(3);
    assert_eq!(app.sort.column, 2);
    assert!(!app.sort.descending, "second press flips to ascending");
    assert_eq!(app.cursor, 4, "same-column press keeps cursor");

    app.apply_sort_key(3);
    assert!(app.sort.descending, "third press flips back to descending");
}

// ---------- 範囲外キーはタブ依存 ----------

#[test]
fn key_9_works_on_upstream_but_not_on_server() {
    // 同一の物理キーがタブによって意味を持ったり持たなかったりするのは
    // 設計上の許容範囲 (footer の表示でユーザーに認知させる)。
    let mut server = app_on_tab(Tab::Server);
    let before = server.sort;
    server.apply_sort_key(9);
    assert_eq!(server.sort, before, "key 9 noop on Server");

    let mut upstream = app_on_tab(Tab::Upstream);
    upstream.apply_sort_key(9);
    assert_eq!(upstream.sort.column, 8, "key 9 → STATE on Upstream");
    assert_eq!(
        column_at(Tab::Upstream, upstream.sort.column),
        Some(SortColumn::State)
    );
}
