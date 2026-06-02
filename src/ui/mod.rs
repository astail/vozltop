//! TUI 描画レイヤのエントリポイント。
//!
//! issue #25 で「最低限の画面が出る」状態の stub `render` を提供したあと、
//! 各 widget は後続 issue で順に埋まる:
//!
//! - **issue #27 (本ファイルの 4 行ヘッダ参照先, #119 で in/out を分離)**: ヘッダ (接続 Gauge + Sparkline)
//! - issue #28-#30: zone テーブル (Server / Upstream / Cache)
//! - issue #31: ソート + フィルタの UI
//! - issue #32: 詳細オーバーレイ
//! - issue #33: footer + help
//!
//! ## レイアウト方針 (issue #27 / #33 / #119 統合後)
//!
//! ```text
//! ┌──────────────────────────────────────────────────────────────────┐
//! │ [Stale] consecutive failures: 2                                  │  <- status banner (Stale/Disconnected のみ)
//! ├──────────────────────────────────────────────────────────────────┤
//! │ Conn  [█████░░░] 42/120   active 42 reading 3 writing 5 …        │  <- header 行 1
//! │ RPS   ▁▂▃▅▇▇▆▄                                          1234/s   │  <- header 行 2
//! │ in    ▁▂▃▅▇▆▄                                         1.2 MB/s   │  <- header 行 3 (#119)
//! │ out   ▁▂▃▅▇▆▄                                         4.5 MB/s   │  <- header 行 4 (#119)
//! ├──────────────────────────────────────────────────────────────────┤
//! │ (本体は後続 issue で実装)                                          │
//! │                                                                   │
//! │                                                                   │
//! ├──────────────────────────────────────────────────────────────────┤
//! │ HTTP 500 Internal Server Error                                    │  <- error banner (任意、1 行)
//! ├──────────────────────────────────────────────────────────────────┤
//! │ F1Help F4Filter F5Sort F10Quit  Tab:Zone Enter  Sort: ZONE ↓     │  <- footer (1 行, #33)
//! └──────────────────────────────────────────────────────────────────┘
//! ```
//!
//! `app.show_help == true` のときは上記レイアウトの上に help モーダル
//! (`src/ui/help.rs`) を `Clear` で重ねて描画する。
//!
//! ## 高さ要件
//!
//! 受け入れ条件「80x24 で崩れない」を満たすため、`Layout::vertical` の
//! `Fill(1)` を本体に置き、極端な resize で本体が 0 行になっても破綻しない
//! 構成にしている。

pub mod detail;
pub mod footer;
pub mod header;
pub mod help;
pub mod host_tab;
pub mod table;

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::state::{App, Workspace};

/// `Workspace` ルートから画面全体を描画する (issue #44)。
///
/// multi-host のときは画面最上段に 1 行の Host タブバーを差し込み、その下に
/// active host の `App` を従来の [`render`] と同じレイアウトで描画する。
/// 単一 host のときは Host タブバーを描画せず、[`render`] と完全に同じ
/// 出力になる (= snapshot / 既存 UI テストが不変)。
pub fn render_workspace(f: &mut Frame<'_>, ws: &Workspace) {
    if !ws.multi() {
        render(f, ws.active());
        return;
    }
    let area = f.area();
    let [host_bar, body] = Layout::vertical([
        Constraint::Length(host_tab::HOST_TAB_HEIGHT),
        Constraint::Fill(1),
    ])
    .areas(area);
    host_tab::render(f, ws, host_bar);
    render_in(f, ws.active(), body);
}

/// app の状態に応じて画面全体を再描画する。
///
/// 副作用は `f` への widget render 呼び出しのみ。I/O を伴わないので
/// `TestBackend` ベースの単体テストで挙動を固定できる。
pub fn render(f: &mut Frame<'_>, app: &App) {
    render_in(f, app, f.area());
}

/// [`render`] の実装本体。area を引数で受けるため、host タブバーぶんの行を
/// 削った領域に描画したい multi-host 経路 ([`render_workspace`]) からも
/// 再利用できる。
pub(crate) fn render_in(f: &mut Frame<'_>, app: &App, area: Rect) {
    let banner_msg = app.error_banner_display();

    // 上から: header(7) → body(fill) → error_banner? → footer(1)
    // issue #150: 旧 status banner 行 (Stale / Disc) はヘッダタイトルの ●
    // ドット + ラベルに統合したため、ここでは行を確保しない。
    let mut constraints: Vec<Constraint> = Vec::with_capacity(4);
    constraints.push(Constraint::Length(header::HEADER_HEIGHT));
    constraints.push(Constraint::Fill(1));
    if banner_msg.is_some() {
        constraints.push(Constraint::Length(1));
    }
    constraints.push(Constraint::Length(1)); // footer

    let rows = Layout::vertical(constraints).split(area);

    let mut idx = 0usize;
    header::render(f, app, rows[idx]);
    idx += 1;
    table::render(f, app, rows[idx]);
    idx += 1;
    if let Some(msg) = banner_msg.as_deref() {
        f.render_widget(banner_widget(msg, app), rows[idx]);
        idx += 1;
    }
    footer::render(f, app, rows[idx]);

    // detail overlay (#32): detail_zone が Some のとき中央に重ねる。
    if app.detail_zone.is_some() {
        detail::render_overlay(f, app, area);
    }

    // help overlay は最後に重ねる (issue #33)。base layout と独立に描く。
    // detail / filter overlay (#31 / #32) とも排他しない設計。
    if app.show_help {
        help::render_overlay(f, area);
    }
}

fn banner_widget<'a>(msg: &'a str, app: &App) -> Paragraph<'a> {
    // `App::error_banner_display` 側で mono 時に "[!] " prefix を付与済み。
    // ここではスタイルだけ載せる。
    let style = if app.theme.mono {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        app.theme.error_banner
    };
    Paragraph::new(Line::from(Span::styled(msg.to_string(), style)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    use crate::state::{App, AppStatus};
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
    fn renders_7_row_rounded_header_with_conn_rps_bw_labels() {
        let app = App::new();
        let out = draw_to_string(&app, 80, 14);
        // ヘッダ (Conn / RPS / IN / OUT)
        assert!(out.contains("Conn"), "out:\n{out}");
        assert!(out.contains("RPS"), "out:\n{out}");
        assert!(out.contains("IN"), "out:\n{out}");
        assert!(out.contains("OUT"), "out:\n{out}");
        // 内部 divider
        assert!(out.contains('┄'), "divider should use ┄; out:\n{out}");
        // footer key hints (#150 で footer は key hint のみに縮約)
        assert!(out.contains("F1Help"), "footer missing F1Help:\n{out}");
    }

    #[test]
    fn help_overlay_only_shown_when_show_help() {
        // show_help=false の通常画面に "key bindings" は出ない
        let mut app = App::new();
        assert!(!app.show_help);
        let normal = draw_to_string(&app, 80, 24);
        assert!(
            !normal.contains("key bindings"),
            "help should NOT appear by default:\n{normal}"
        );

        // show_help=true でモーダルが出現
        app.show_help = true;
        let with_help = draw_to_string(&app, 80, 24);
        assert!(
            with_help.contains("key bindings"),
            "help should appear when show_help:\n{with_help}"
        );
        // letter alias 案内も入る
        assert!(
            with_help.contains("? = F1"),
            "help should include letter alias:\n{with_help}"
        );
    }

    #[test]
    fn stale_status_appears_in_header_title() {
        // issue #150: 旧 pre-header status banner はヘッダタイトル (●ドット +
        // "Stale (N)" ラベル) に統合された。
        let mut app = App::new();
        app.status = AppStatus::Stale {
            last_ok: std::time::Instant::now(),
            failures: 2,
        };
        let out = draw_to_string(&app, 80, 14);
        assert!(out.contains("Stale"), "out:\n{out}");
        assert!(out.contains("(2)"), "failure count in title; out:\n{out}");
        // ヘッダ本体も併存
        assert!(out.contains("Conn"), "out:\n{out}");
        assert!(out.contains("RPS"), "out:\n{out}");
    }

    #[test]
    fn disconnected_status_appears_in_header_title() {
        let mut app = App::new();
        app.status = AppStatus::Disconnected { failures: 5 };
        let out = draw_to_string(&app, 80, 14);
        assert!(out.contains("Disconnected"), "out:\n{out}");
        assert!(out.contains("(5)"), "out:\n{out}");
    }

    #[test]
    fn running_state_does_not_show_legacy_status_banner_strings() {
        let mut app = App::new();
        app.status = AppStatus::Running;
        let out = draw_to_string(&app, 80, 14);
        // 旧 banner の文字列は出てこない
        assert!(!out.contains("[Stale]"), "out:\n{out}");
        assert!(!out.contains("[Disconnected]"), "out:\n{out}");
        // ヘッダはそのまま表示
        assert!(out.contains("Conn"), "out:\n{out}");
    }

    #[test]
    fn error_banner_is_rendered_when_present() {
        let mut app = App::new();
        app.error_banner = Some("HTTP 500 Internal Server Error".to_string());
        let out = draw_to_string(&app, 80, 14);
        assert!(out.contains("HTTP 500"), "out:\n{out}");
    }

    #[test]
    fn error_banner_has_bang_prefix_in_mono_theme() {
        let mut app = App::with_theme(Theme::mono());
        app.error_banner = Some("HTTP 500 Internal Server Error".to_string());
        let out = draw_to_string(&app, 80, 14);
        assert!(out.contains("[!] HTTP 500"), "out:\n{out}");
    }

    #[test]
    fn renders_without_banner_when_no_error() {
        let app = App::new();
        let out = draw_to_string(&app, 80, 14);
        // 通常時はヘッダの 5 行 + body + footer
        assert!(out.contains("Conn"), "out:\n{out}");
        // footer 由来の F1 hint
        assert!(out.contains("F1Help"), "out:\n{out}");
    }

    #[test]
    fn tiny_terminal_does_not_panic() {
        // header 3 行 + footer 1 行 = 4 行未満でも panic しない (layout が
        // 下から切り捨てる)
        let app = App::new();
        let _ = draw_to_string(&app, 20, 2);
    }

    #[test]
    fn fits_in_80x24_layout() {
        // 受け入れ条件: 80x24 で崩れない
        let app = App::new();
        let _ = draw_to_string(&app, 80, 24);
    }
}
