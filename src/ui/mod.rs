//! TUI 描画レイヤのエントリポイント。
//!
//! 本ファイルは issue #25 で「最低限の画面が出る」状態まで持っていくための
//! stub `render` を提供する。実際の widget 構成 (header / table / footer /
//! detail / help) は後続 issue で埋めていく:
//!
//! - issue #27: ヘッダ (接続 Gauge + Sparkline)
//! - issue #28-#30: zone テーブル (Server / Upstream / Cache)
//! - issue #31: ソート + フィルタの UI
//! - issue #32: 詳細オーバーレイ
//! - issue #33: footer + help
//!
//! 本 PR のレイアウト方針:
//!
//! ```text
//! ┌──────────────────────────────────────────────────────────┐
//! │ vozltop 0.1.0 — http://host/status/format/json  [Connecting] │  <- header (1 行)
//! ├──────────────────────────────────────────────────────────┤
//! │ (本体は後続 issue で実装)                                  │
//! │                                                           │
//! │                                                           │
//! ├──────────────────────────────────────────────────────────┤
//! │ HTTP 500 Internal Server Error                            │  <- error banner (任意、1 行)
//! ├──────────────────────────────────────────────────────────┤
//! │ q/F10 Quit                                                │  <- footer (1 行)
//! └──────────────────────────────────────────────────────────┘
//! ```
//!
//! header / footer / banner のテキストは本 PR で確定させ、後続 issue では
//! ボディの中身 (Table / Sparkline 等) を埋めていく。

use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::state::{App, AppStatus};

/// app の状態に応じて画面全体を再描画する。
///
/// 引数:
///
/// - `f`: ratatui の `Frame`。今フレームの描画先。
/// - `app`: アプリ状態 (read-only)。`status` と `error_banner` から
///   header / banner のテキストを生成する。
///
/// 副作用は `f` への widget render 呼び出しのみ。
/// I/O を伴わないので `TestBackend` ベースの単体テストで挙動を固定できる。
pub fn render(f: &mut Frame<'_>, app: &App) {
    // banner の有無で「中段」高さが 1 行ぶん変わるため、レイアウトを分岐する。
    // Length(1) を 3 つ並べると、リサイズで「本体が 0 行」になっても破綻しない
    // ように Fill(1) を本体に置く構成。
    let banner = app.error_banner_display();

    let area = f.area();
    if let Some(msg) = banner.as_deref() {
        let [header_area, body_area, banner_area, footer_area] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Fill(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(area);
        f.render_widget(header_widget(app), header_area);
        f.render_widget(body_placeholder(), body_area);
        f.render_widget(banner_widget(msg, app), banner_area);
        f.render_widget(footer_widget(), footer_area);
    } else {
        let [header_area, body_area, footer_area] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Fill(1),
            Constraint::Length(1),
        ])
        .areas(area);
        f.render_widget(header_widget(app), header_area);
        f.render_widget(body_placeholder(), body_area);
        f.render_widget(footer_widget(), footer_area);
    }
}

fn header_widget(app: &App) -> Paragraph<'static> {
    let status_label = match &app.status {
        AppStatus::Connecting => "Connecting",
        AppStatus::Running => "Running",
        AppStatus::Stale { .. } => "Stale",
        AppStatus::Disconnected { .. } => "Disconnected",
    };
    let title = concat!(env!("CARGO_PKG_NAME"), " ", env!("CARGO_PKG_VERSION"));
    let line = Line::from(vec![
        Span::styled(title, Style::default().add_modifier(Modifier::BOLD)),
        Span::raw("  ["),
        Span::raw(status_label),
        Span::raw("]"),
    ]);
    Paragraph::new(line)
}

fn body_placeholder() -> Paragraph<'static> {
    // 後続 issue (#27-#30) で widget が埋まるまでの暫定表示。テキストは固定で、
    // 「画面が出ている」ことを確認するための最低限のヒント。
    Paragraph::new(Line::from(Span::styled(
        "waiting for first VTS snapshot…",
        Style::default().add_modifier(Modifier::DIM),
    )))
}

fn banner_widget<'a>(msg: &'a str, app: &App) -> Paragraph<'a> {
    let style = if app.theme.mono {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        // 色付き時のスタイルは #26 で Theme に集約されているが、ここでは
        // 「error っぽく見える」最低限の差別化として bold + 既定色を使う。
        // 色そのものは Theme 拡張時に集約する (issue #27 以降)。
        Style::default().add_modifier(Modifier::BOLD)
    };
    Paragraph::new(Line::from(Span::styled(msg.to_string(), style)))
}

fn footer_widget() -> Paragraph<'static> {
    // CLAUDE.md のキー割り当てに従って最低限のヒントだけ出す。完全な help は
    // F1 / ? overlay (#33) で別途実装する。
    Paragraph::new(Line::from(Span::styled(
        "q / F10  Quit",
        Style::default().add_modifier(Modifier::DIM),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    use crate::state::App;
    use crate::theme::Theme;

    fn draw_to_string(app: &App, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| render(f, app)).expect("draw");
        let buf = terminal.backend().buffer().clone();
        let mut s = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                s.push_str(buf[(x, y)].symbol());
            }
            s.push('\n');
        }
        s
    }

    #[test]
    fn header_shows_app_name_version_and_connecting_state() {
        let app = App::new();
        let out = draw_to_string(&app, 80, 6);
        // CARGO_PKG_NAME と VERSION の両方を含むこと
        assert!(out.contains(env!("CARGO_PKG_NAME")), "out:\n{out}");
        assert!(out.contains(env!("CARGO_PKG_VERSION")), "out:\n{out}");
        // 初期状態は Connecting
        assert!(out.contains("Connecting"), "out:\n{out}");
        // body プレースホルダ
        assert!(
            out.contains("waiting for first VTS snapshot"),
            "out:\n{out}"
        );
        // footer
        assert!(out.contains("Quit"), "out:\n{out}");
    }

    #[test]
    fn header_status_label_reflects_running_state() {
        let mut app = App::new();
        app.status = AppStatus::Running;
        let out = draw_to_string(&app, 60, 4);
        assert!(out.contains("Running"), "out:\n{out}");
        // 切り替え後は Connecting ラベルが消えていること
        assert!(!out.contains("Connecting"), "out:\n{out}");
    }

    #[test]
    fn error_banner_is_rendered_when_present() {
        let mut app = App::new();
        app.error_banner = Some("HTTP 500 Internal Server Error".to_string());
        let out = draw_to_string(&app, 80, 6);
        assert!(out.contains("HTTP 500"), "out:\n{out}");
    }

    #[test]
    fn error_banner_has_bang_prefix_in_mono_theme() {
        // theme = mono のとき、banner は "[!] " が前置される (state::App の責務)
        let mut app = App::with_theme(Theme::mono());
        app.error_banner = Some("HTTP 500 Internal Server Error".to_string());
        let out = draw_to_string(&app, 80, 6);
        assert!(out.contains("[!] HTTP 500"), "out:\n{out}");
    }

    #[test]
    fn renders_without_banner_when_no_error() {
        let app = App::new();
        let out = draw_to_string(&app, 80, 5);
        // 各種ラベルが出ていれば OK (banner 行を持たない 3 段レイアウト)
        assert!(out.contains("Connecting"), "out:\n{out}");
        assert!(out.contains("Quit"), "out:\n{out}");
    }

    #[test]
    fn tiny_terminal_does_not_panic() {
        // リサイズで極端に小さくしても panic しない (Layout が高さを切り詰める)
        let app = App::new();
        // header(1) + body(>=0) + footer(1) = 最小 2 行
        let _ = draw_to_string(&app, 20, 2);
    }
}
