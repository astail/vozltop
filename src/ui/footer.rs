//! 画面下端の常時 footer (issue #33)。
//!
//! 左側: キー割り当てのヒント (htop 風)。
//! 右側: 現在のソート列 / 方向、Filter 状態。
//!
//! 80x24 で 1 行に収めるため、表記は短く保つ:
//!
//! ```text
//! F1Help F4Filter F5Sort F10Quit  Tab:NextZone Enter:Detail            Sort: Col1 ↓  Filter: foo
//! ```
//!
//! letter alias (`?`, `/`, `q`) は help overlay (F1) 側で案内する。footer の
//! ヒントは F-key の表記に統一し、ターミナル幅を圧迫しないようにする。
//!
//! v1 未実装の `F2 Setup` / `F3` は **掲載しない** (#33 受け入れ条件)。

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::state::{App, Tab};

/// キー割り当てヒント。`render` が左寄せで表示する固定文字列。
///
/// 80 桁前提でも収まるよう 60 桁以内に抑える (右側のステータスと干渉しない)。
pub const KEY_HINTS: &str = "F1Help F4Filter F5Sort F10Quit  Tab:NextZone Enter:Detail";

/// 画面下端 1 行に footer を描画する。
pub fn render(f: &mut Frame<'_>, app: &App, area: Rect) {
    f.render_widget(footer_paragraph(app), area);
}

/// footer の `Paragraph` を組み立てる (テスト用に切り出し)。
fn footer_paragraph(app: &App) -> Paragraph<'_> {
    // 左ヒント / 右ステータスを 1 行に並べる。ratatui の `Paragraph` は
    // alignment が一括指定のため、左ヒント・スペーサー・右ステータスを
    // Span として連結し、ターミナル幅に依存しない簡易構成にする。
    let mut spans: Vec<Span<'_>> = Vec::with_capacity(6);
    spans.push(Span::styled(KEY_HINTS, Style::default()));
    spans.push(Span::raw("  "));
    spans.push(Span::styled(sort_label(app), Style::default()));
    if !app.filter.is_empty() {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            filter_label(&app.filter),
            Style::default().add_modifier(Modifier::BOLD),
        ));
    }
    Paragraph::new(Line::from(spans)).style(Style::default().add_modifier(Modifier::DIM))
}

/// 現在のソート列 / 方向を `"Sort: <col> ↓"` 形式で表す。
fn sort_label(app: &App) -> String {
    let arrow = if app.sort.descending { "↓" } else { "↑" };
    let col = column_name(app.active_tab, app.sort.column);
    format!("Sort: {col} {arrow}")
}

/// `"Filter: <substr>"`。長い文字列は 24 文字で省略する。
fn filter_label(filter: &str) -> String {
    const MAX: usize = 24;
    if filter.chars().count() > MAX {
        let truncated: String = filter.chars().take(MAX).collect();
        format!("Filter: {truncated}…")
    } else {
        format!("Filter: {filter}")
    }
}

/// タブごとの列名。`SortState::column` (0-based) を人間が読めるラベルに変換。
///
/// 各タブの列構成は本 PR (#33) 時点では未確定 (#28-#30 で確定) なので、最低限
/// 0 番列 = ZONE と、不明な列に対するフォールバックだけ実装。後続 issue で
/// 具体的なマッピングが入る。
fn column_name(_tab: Tab, column: u8) -> &'static str {
    match column {
        0 => "ZONE",
        n => match n {
            1 => "Col1",
            2 => "Col2",
            3 => "Col3",
            4 => "Col4",
            5 => "Col5",
            6 => "Col6",
            7 => "Col7",
            8 => "Col8",
            9 => "Col9",
            _ => "?",
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{App, SortState, Tab};

    #[test]
    fn key_hints_fit_in_80_columns() {
        // 右側のソートステータス + スペーサーも含めて 80 桁に収めるための上限。
        // 受け入れ条件: 80x24 で footer が 1 行に収まる。
        assert!(
            KEY_HINTS.chars().count() <= 60,
            "KEY_HINTS too long ({}): {KEY_HINTS}",
            KEY_HINTS.chars().count()
        );
    }

    #[test]
    fn key_hints_mention_all_required_keys() {
        // CLAUDE.md / issue #33 受け入れ: F1 Help / F4 Filter / F5 Sort / F10 Quit /
        // Tab / Enter は全て掲載すること。
        for token in ["F1", "F4", "F5", "F10", "Tab", "Enter", "Help", "Quit"] {
            assert!(
                KEY_HINTS.contains(token),
                "KEY_HINTS missing {token}: {KEY_HINTS}"
            );
        }
    }

    #[test]
    fn key_hints_do_not_advertise_unimplemented_keys() {
        // v1 では F2 / F3 を未実装にしているため、footer に出さない。
        // 大文字 "F2"/"F3" の **語尾境界** に注意 (F20 等は無いがある日入る可能性)。
        // 単純化のため `F2` / `F3` の直接出現を NG にする (F20 系 v1 未予定)。
        assert!(!KEY_HINTS.contains("F2"), "F2 should not appear");
        assert!(!KEY_HINTS.contains("F3"), "F3 should not appear");
    }

    #[test]
    fn sort_label_shows_descending_arrow_by_default() {
        let app = App::new();
        assert_eq!(sort_label(&app), "Sort: ZONE ↓");
    }

    #[test]
    fn sort_label_shows_ascending_arrow_when_not_descending() {
        let mut app = App::new();
        app.sort = SortState {
            column: 1,
            descending: false,
        };
        assert_eq!(sort_label(&app), "Sort: Col1 ↑");
    }

    #[test]
    fn filter_label_shows_filter_string_verbatim() {
        assert_eq!(filter_label("foo"), "Filter: foo");
    }

    #[test]
    fn filter_label_truncates_overly_long_strings() {
        let very_long = "a".repeat(50);
        let got = filter_label(&very_long);
        assert!(got.starts_with("Filter: "));
        assert!(got.ends_with('…'), "truncation marker missing: {got}");
    }

    #[test]
    fn column_name_zero_is_zone_for_all_tabs() {
        for tab in [Tab::Server, Tab::Upstream, Tab::Cache] {
            assert_eq!(column_name(tab, 0), "ZONE");
        }
    }
}
