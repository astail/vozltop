//! Host タブバー (issue #44 / 案 A)。
//!
//! `Workspace.multi()` のとき、画面最上段に 1 行の host タブバーを描画する。
//! 単一 host 起動時は呼ばれない (UI mod 側で分岐)。
//!
//! ## レイアウト
//!
//! ```text
//! HOST  [web-prod-1]  web-prod-2  edge-tokyo  api-asia⚠
//! ```
//!
//! - `HOST` ラベル (5 文字) + 各 host 名 + active を `[...]` で括る
//! - active host は `theme.row_selected` のスタイル
//! - 各 host のアラート (`App.alert_active`) は host 名末尾に `⚠` バッジ
//!   (mono mode では `(!)` に置換、Unicode 非対応端末向け)
//!
//! ## 設計判断
//!
//! - **ratatui の `Tabs` widget を使わない**: `Tabs` は枠線 / 区切り文字
//!   (`│`) を強制し、`Block` の枠線無しでも tab 間の divider が出てしまう。
//!   1 行ヘッダの htop 風 UI には過剰なので、`Paragraph` で span を組み立てて
//!   描画する。
//! - **アラートバッジは host 単位**: 各 `App.alert_active` を読む。host タブを
//!   切り替えなくても他 host のアラートに気付ける (`⚠` を見て切替)。

use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::state::Workspace;
use crate::theme::Theme;

/// Host タブバーの高さ (固定 1 行)。
pub const HOST_TAB_HEIGHT: u16 = 1;

/// `area` の 1 行に host タブバーを描画する。
///
/// `area.width` が極端に狭い場合は ratatui が右端で clip する。
pub fn render(f: &mut Frame<'_>, ws: &Workspace, area: Rect) {
    let theme = active_theme(ws);
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(2 + ws.host_count() * 2);
    spans.push(Span::styled("HOST  ", theme.header_label));

    let active_idx = ws.active_index();
    // issue #44 レビュー指摘: `ws.iter().enumerate()` で 1 度回す (旧版は
    // `host_ids` を回しながら `ws.iter().nth(idx)` で alert を引いていて O(N²))。
    for (idx, (id, app)) in ws.iter().enumerate() {
        if idx > 0 {
            spans.push(Span::raw("  "));
        }
        let alerting = app.alert_active;
        let badge = if alerting {
            if theme.mono {
                "(!)"
            } else {
                "⚠"
            }
        } else {
            ""
        };
        let label = if idx == active_idx {
            format!("[{id}]{badge}")
        } else {
            format!("{id}{badge}")
        };
        // issue #44 レビュー指摘: active かつ alerting のときは `row_selected`
        // (REVERSED) だけだと warn 色が消えるため、`theme.alert` (赤反転) を使う。
        // 表の `row_is_alerting` と同じスタイル → 画面全体で「アラート = 赤系」が
        // 揃う。`⚠` バッジは残るが、色も伝えることで気付きやすさを上げる。
        let style = match (idx == active_idx, alerting) {
            (true, true) => theme.alert,
            (true, false) => theme.row_selected,
            (false, true) => theme.status_warn,
            (false, false) => Style::default(),
        };
        spans.push(Span::styled(label, style));
    }

    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// active App のテーマ。Workspace は host 単位の theme を持たないので
/// active host のテーマを bar 全体に適用する (theme は CLI 共通のため
/// 実質的に同じ)。
fn active_theme(ws: &Workspace) -> &Theme {
    &ws.active().theme
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    use crate::state::App;
    use crate::theme::Theme;

    fn draw_to_string(ws: &Workspace, w: u16) -> String {
        let backend = TestBackend::new(w, 1);
        let mut terminal = Terminal::new(backend).expect("term");
        terminal.draw(|f| render(f, ws, f.area())).expect("draw");
        let buf = terminal.backend().buffer().clone();
        let mut s = String::new();
        for x in 0..buf.area.width {
            s.push_str(buf[(x, 0)].symbol());
        }
        s
    }

    #[test]
    fn renders_host_label_and_each_id() {
        let ws = Workspace::new(vec![
            ("prod".to_string(), App::new()),
            ("staging".to_string(), App::new()),
        ]);
        let out = draw_to_string(&ws, 80);
        assert!(out.contains("HOST"), "out: {out}");
        assert!(out.contains("prod"), "out: {out}");
        assert!(out.contains("staging"), "out: {out}");
    }

    #[test]
    fn active_host_is_bracketed() {
        let ws = Workspace::new(vec![
            ("a".to_string(), App::new()),
            ("b".to_string(), App::new()),
        ]);
        let out = draw_to_string(&ws, 40);
        assert!(out.contains("[a]"), "active host in brackets: {out}");
        // 非 active には bracket がつかない
        assert!(
            !out.contains("[b]"),
            "inactive host without brackets: {out}"
        );
    }

    #[test]
    fn alerting_host_shows_warning_badge() {
        let mut ws = Workspace::new(vec![
            ("a".to_string(), App::new()),
            ("b".to_string(), App::new()),
        ]);
        ws.app_mut("b").unwrap().alert_active = true;
        let out = draw_to_string(&ws, 40);
        // color theme: ⚠ Unicode、mono theme: (!) を別途検証
        assert!(out.contains("⚠"), "alert badge present: {out}");
    }

    #[test]
    fn active_and_alerting_host_renders_both_bracket_and_badge() {
        // issue #44 レビュー指摘: active かつ alerting のとき、ブラケット (active 印)
        // と ⚠ バッジ (alerting 印) の両方が出る + 色が theme.alert (= row_selected が
        // status_warn を上書きしない) であることを担保する。
        let mut ws = Workspace::new(vec![
            ("a".to_string(), App::new()),
            ("b".to_string(), App::new()),
        ]);
        // active host (idx=0) に alert を立てる
        ws.app_mut("a").unwrap().alert_active = true;
        let out = draw_to_string(&ws, 40);
        assert!(out.contains("[a]"), "active bracket present: {out}");
        assert!(
            out.contains("[a]⚠"),
            "active + alerting host shows both `[...]` and `⚠`: {out}"
        );
    }

    #[test]
    fn alerting_uses_text_badge_in_mono_theme() {
        let mut ws = Workspace::new(vec![
            ("a".to_string(), App::with_theme(Theme::mono())),
            ("b".to_string(), App::with_theme(Theme::mono())),
        ]);
        ws.app_mut("b").unwrap().alert_active = true;
        let out = draw_to_string(&ws, 40);
        assert!(out.contains("(!)"), "mono: text badge: {out}");
        assert!(!out.contains("⚠"), "mono: no unicode badge: {out}");
    }

    #[test]
    fn narrow_width_does_not_panic() {
        let ws = Workspace::new(vec![
            ("a".to_string(), App::new()),
            ("b".to_string(), App::new()),
        ]);
        let _ = draw_to_string(&ws, 5);
    }
}
