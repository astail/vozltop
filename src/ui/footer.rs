//! 画面下端の常時 footer (issue #33 / #150)。
//!
//! キー割り当てヒントだけを左寄せで表示する 1 行 widget。
//!
//! ```text
//! F1Help F4Filter F5Sort F10Quit  Tab:Zone Enter:Detail
//! ```
//!
//! ## issue #150 での変更
//!
//! 旧 footer は右側に `Sort: <col> ↓` / `Filter: <q>` を出していたが、これは
//! table title bar (`Server Zones · 3/47 · filter "api"`) と列見出しの sort
//! 矢印 (`RPS↓`) に集約されたため、footer は **キーヒントだけに縮約** した。
//!
//! letter alias (`?`, `/`, `q`) は help overlay (F1) 側で案内する。

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use ratatui::layout::Rect;

use crate::state::App;

/// キー割り当てヒント。
pub const KEY_HINTS: &str = "F1Help F4Filter F5Sort F10Quit  Tab:Zone Enter:Detail";

/// 画面下端 1 行に footer を描画する (キーヒントのみ)。
pub fn render(f: &mut Frame<'_>, app: &App, area: Rect) {
    let style = if app.theme.mono {
        Style::default().add_modifier(Modifier::DIM)
    } else {
        app.theme.footer
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(KEY_HINTS, style))),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::App;

    #[test]
    fn key_hints_fit_in_80_columns() {
        assert!(
            KEY_HINTS.chars().count() <= 80,
            "KEY_HINTS too long ({}) for 80-col terminal: {KEY_HINTS}",
            KEY_HINTS.chars().count()
        );
    }

    #[test]
    fn key_hints_mention_all_required_keys() {
        for token in ["F1", "F4", "F5", "F10", "Tab", "Enter", "Help", "Quit"] {
            assert!(
                KEY_HINTS.contains(token),
                "KEY_HINTS missing {token}: {KEY_HINTS}"
            );
        }
    }

    #[test]
    fn key_hints_do_not_advertise_unimplemented_keys() {
        assert!(!KEY_HINTS.contains("F2"), "F2 should not appear");
        assert!(!KEY_HINTS.contains("F3"), "F3 should not appear");
    }

    /// 80 桁ターミナルで keyhint が 1 行に収まることを確認する。
    /// Sort / Filter は title bar に移ったので footer には出ない。
    #[test]
    fn rendered_footer_fits_within_80_columns_without_truncation() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let app = App::new();
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

        for token in ["F1Help", "F4Filter", "F5Sort", "F10Quit"] {
            assert!(
                line.contains(token),
                "footer at 80 cols should include `{token}`:\n[{line}]"
            );
        }
    }
}
