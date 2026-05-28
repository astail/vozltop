//! 画面下端の常時 footer (issue #33)。
//!
//! 左側 (`hints` 領域): キー割り当てのヒント (htop 風)。
//! 右側 (`status` 領域): 現在のソート列 / 方向、Filter 状態。
//!
//! 80x24 で 1 行に収めるため、`Layout::horizontal` で固定幅に分割する:
//!
//! ```text
//! F1Help F4Filter F5Sort F10Quit  Tab:Zone Enter:Detail     Sort: ZONE ↓ Filter: abc
//! ├──────────── hints (Min 40 桁) ──────────────────────────┤├── status (Length 30) ─┤
//! ```
//!
//! status 領域の幅を超える長い Filter は ratatui の Paragraph が右端で
//! truncate する (改行はしない)。詳細を確認したい場合は F1 / `?` の
//! help モーダルを参照する設計。
//!
//! letter alias (`?`, `/`, `q`) は help overlay (F1) 側で案内する。footer の
//! ヒントは F-key の表記に統一し、ターミナル幅を圧迫しないようにする。
//!
//! v1 未実装の `F2 Setup` / `F3` は **掲載しない** (#33 受け入れ条件)。

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::state::{column_label, App};

/// キー割り当てヒント。`render` が左寄せで表示する固定文字列。
///
/// 80 桁前提で hints 領域 (T - 30 桁) に収まるよう 50 桁以下に保つ。
/// 80x24 のとき hints = 50 桁、status = 30 桁。spec の `Tab:NextZone Enter:Detail`
/// は 80 桁では truncate するため、`Tab:Zone Enter` に短縮し help modal で
/// 詳細補足する設計 (issue #33 PR セルフレビュー)。
pub const KEY_HINTS: &str = "F1Help F4Filter F5Sort F10Quit  Tab:Zone Enter";

/// 画面下端 1 行に footer を描画する。
///
/// 左側 (Min 40 桁) にキー hint、右側 (Length 30 桁) にソート / フィルタ状態。
/// 80 桁未満のときは status 側が縮み、最終的に truncate される。
pub fn render(f: &mut Frame<'_>, app: &App, area: Rect) {
    // hints と status を Layout::horizontal で分離する。これにより:
    // - 80 桁時: hints 50 / status 30 で sort+filter が両方収まる
    // - 100 桁時: hints 70 / status 30 で余白が hints 側に行く
    // - 狭いターミナル時: hints が縮み、必要なら status が clip される
    let [hints_area, status_area] =
        Layout::horizontal([Constraint::Min(40), Constraint::Length(30)]).areas(area);
    f.render_widget(hints_paragraph(), hints_area);
    f.render_widget(status_paragraph(app), status_area);
}

/// 左側: 固定キー hint。
fn hints_paragraph() -> Paragraph<'static> {
    Paragraph::new(Line::from(Span::raw(KEY_HINTS)))
        .style(Style::default().add_modifier(Modifier::DIM))
}

/// 右側: ソート + Filter 状態。Filter が非空なら追加表示。
fn status_paragraph(app: &App) -> Paragraph<'_> {
    let mut spans: Vec<Span<'_>> = Vec::with_capacity(3);
    spans.push(Span::raw(sort_label(app)));
    if !app.filter.is_empty() {
        spans.push(Span::raw(" "));
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
    let col = column_label(app.active_tab, app.sort.column);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{App, SortState, Tab};

    #[test]
    fn key_hints_fit_in_80_columns() {
        // Layout::horizontal で hints (Min 40) + status (Length 30) に分けるため、
        // 80 桁時の hints 領域は 50 桁。KEY_HINTS が 50 桁を超えると右端で truncate。
        // 受け入れ条件 "80x24 で footer が 1 行に収まる" を実質満たすため 50 桁以下に。
        assert!(
            KEY_HINTS.chars().count() <= 50,
            "KEY_HINTS too long ({}) for 80-col hints region: {KEY_HINTS}",
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
        // Server タブ col 1 = RPS (issue #31 の列マッピング)。
        app.sort = SortState {
            column: 1,
            descending: false,
        };
        assert_eq!(sort_label(&app), "Sort: RPS ↑");
    }

    #[test]
    fn sort_label_uses_tab_specific_column_names() {
        // 同じ column index でもタブで列名が変わる (issue #31)。
        let mut app = App::new();
        app.sort = SortState {
            column: 8,
            descending: true,
        };
        app.active_tab = Tab::Upstream;
        // Upstream col 8 (= 9 番目) は STATE。
        assert_eq!(sort_label(&app), "Sort: STATE ↓");
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
    fn sort_label_zero_is_zone_for_all_tabs() {
        let mut app = App::new();
        app.sort = SortState {
            column: 0,
            descending: true,
        };
        for tab in [Tab::Server, Tab::Upstream, Tab::Cache] {
            app.active_tab = tab;
            assert_eq!(sort_label(&app), "Sort: ZONE ↓");
        }
    }

    /// 80 桁ターミナルで全ての情報 (キーヒント + Sort + Filter) が
    /// 1 行に収まり truncate されないことを TestBackend で検証する。
    /// セルフレビュー指摘 (PR #91) に対する回帰防止テスト。
    #[test]
    fn rendered_footer_fits_within_80_columns_without_truncation() {
        use ratatui::backend::TestBackend;
        use ratatui::layout::Rect;
        use ratatui::Terminal;

        let mut app = App::new();
        app.filter = "api".to_string(); // 短めの典型的な filter

        let backend = TestBackend::new(80, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| super::render(f, &app, Rect::new(0, 0, 80, 1)))
            .unwrap();

        let buf = terminal.backend().buffer().clone();
        let mut line = String::new();
        for x in 0..buf.area.width {
            line.push_str(buf[(x, 0)].symbol());
        }

        // 必須トークンが全て出現すること
        for token in [
            "F1Help",
            "F4Filter",
            "F5Sort",
            "F10Quit",
            "Sort:",
            "Filter: api",
        ] {
            assert!(
                line.contains(token),
                "footer at 80 cols should include `{token}`:\n[{line}]"
            );
        }
    }
}
