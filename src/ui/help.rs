//! F1 / `?` でトグルする help モーダル (issue #33)。
//!
//! 画面中央にキー対応表を描画する。表示有無は `App::show_help` で判断し、
//! `App::show_help = true` のときに [`render_overlay`] が直前の画面に重ねて描画する。
//!
//! 内容は CLAUDE.md / README のキー割り当てと一致させる:
//!
//! - F1 / `?`: help open/close
//! - Tab / Shift+Tab: zone 種別切替
//! - ↑↓ / k j: 行カーソル移動
//! - PgUp / PgDn: ページ送り
//! - Enter: 詳細オーバーレイ
//! - Esc: 詳細 / フィルタ解除 / help を閉じる
//! - F4 / `/`: フィルタ
//! - F5: ソート方向反転
//! - 1-9: ソート列指定
//! - F10 / q / Ctrl-C: 終了
//!
//! macOS Terminal.app は F1-F4 を OS 側で奪うため、letter alias
//! (`?` = F1, `/` = F4, `q` = F10) を必ず明記する (#33 受け入れ条件)。

use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

/// help モーダルの行構成。各タプルは `(キー表記, 説明)`。
///
/// 表として見たときの整形は [`render_overlay`] が `format!` で 1 行ずつ
/// 行う。固定幅で 12 桁 + 説明にする。
const HELP_ROWS: &[(&str, &str)] = &[
    ("F1 / ?", "open or close this help"),
    (
        "Tab / S-Tab",
        "switch zone type (Server / Upstream / Cache / Filter)",
    ),
    ("Up / Down", "move row cursor (also: k / j)"),
    ("PgUp / PgDn", "page up / down"),
    ("Enter", "open zone detail overlay"),
    ("Esc", "close detail / clear filter / close help"),
    ("F4 / /", "filter zones by substring"),
    ("F5", "reverse sort direction"),
    ("1 – 9", "choose sort column (tab-specific)"),
    ("F10 / q / Ctrl-C", "quit"),
];

/// help overlay の固定タイトル。
const HELP_TITLE: &str = " vozltop — key bindings ";

/// macOS Terminal.app 向けの letter alias 案内。表の下に 2 行で添える。
const NOTE_LINE_1: &str = "macOS Terminal.app intercepts F1-F4; use letter aliases:";
const NOTE_LINE_2: &str = "  ? = F1   / = F4   q = F10";

/// オーバーレイとして help を描く。
///
/// `frame_area` は画面全体。中央寄せのモーダル領域を計算し、`Clear` を打ってから
/// `Block::bordered` の中にキー表を `Paragraph` で流し込む。
///
/// 注意: 呼び出し側 (`ui::render`) で `app.show_help == true` のときだけ呼ぶこと。
pub fn render_overlay(f: &mut Frame<'_>, frame_area: Rect) {
    let area = centered_rect(frame_area);
    f.render_widget(Clear, area);
    f.render_widget(help_block(), area);
}

/// help を `Block::bordered` + `Paragraph` で組み立てる。テスト用に分離。
fn help_block() -> Paragraph<'static> {
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(HELP_ROWS.len() + 3);
    for (key, desc) in HELP_ROWS {
        // キーは 18 桁固定 (最長 "F10 / q / Ctrl-C" が 16 桁、余裕を見て 18)。
        let padded = format!("{key:<18}");
        lines.push(Line::from(vec![
            Span::styled(padded, Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(*desc),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        NOTE_LINE_1,
        Style::default().add_modifier(Modifier::DIM),
    )));
    lines.push(Line::from(Span::styled(
        NOTE_LINE_2,
        Style::default().add_modifier(Modifier::DIM),
    )));

    Paragraph::new(lines)
        .alignment(Alignment::Left)
        .block(Block::default().borders(Borders::ALL).title(HELP_TITLE))
}

/// help モーダルの矩形を画面中央に配置する。
///
/// 固定サイズの矩形を中央に置く: 60 桁 × 16 行 (タイトル+枠で +2、表 10 行+
/// 空行+注記 2 行 = 13 行、余白を入れて 16 行)。フレームが小さい場合は
/// `min(frame_size, target_size)` でクランプする。
fn centered_rect(frame: Rect) -> Rect {
    // 幅 74: 80 桁ターミナル前提で内側 72 桁 (border 2 を差し引く)。
    // 18 桁の key 列 + 最長 description
    // "switch zone type (Server / Upstream / Cache / Filter)" (53 桁) が 1 行に
    // 収まる必要があるため、合計 71 桁。70 では右端で truncate するので 74 にする
    // (issue #45 で Filter タブを追加した際に description が伸びた)。
    const TARGET_WIDTH: u16 = 74;
    const TARGET_HEIGHT: u16 = 16;

    let width = TARGET_WIDTH.min(frame.width);
    let height = TARGET_HEIGHT.min(frame.height);

    let [_, mid_v, _] = Layout::vertical([
        Constraint::Length((frame.height.saturating_sub(height)) / 2),
        Constraint::Length(height),
        Constraint::Min(0),
    ])
    .areas(frame);

    let [_, center, _] = Layout::horizontal([
        Constraint::Length((frame.width.saturating_sub(width)) / 2),
        Constraint::Length(width),
        Constraint::Min(0),
    ])
    .areas(mid_v);

    center
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn draw_help_to_string(width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| render_overlay(f, f.area()))
            .expect("draw");
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
    fn help_contains_all_documented_keys() {
        let out = draw_help_to_string(80, 24);
        // 表の主要キーが描画されていること。
        for token in [
            "F1", "?", "Tab", "Up", "Down", "PgUp", "PgDn", "Enter", "Esc", "F4", "/", "F5", "F10",
            "q", "Ctrl-C",
        ] {
            assert!(
                out.contains(token),
                "help should mention {token}\n--- out ---\n{out}"
            );
        }
    }

    #[test]
    fn help_mentions_zone_switch_semantics() {
        let out = draw_help_to_string(80, 24);
        assert!(out.contains("Server"), "out:\n{out}");
        assert!(out.contains("Upstream"), "out:\n{out}");
        assert!(out.contains("Cache"), "out:\n{out}");
        assert!(out.contains("Filter"), "out:\n{out}");
    }

    #[test]
    fn help_includes_macos_letter_alias_note() {
        // 受け入れ条件: letter alias (?,/,q) を明記
        let out = draw_help_to_string(80, 24);
        assert!(out.contains("macOS"), "out:\n{out}");
        assert!(out.contains("? = F1"), "out:\n{out}");
        assert!(out.contains("/ = F4"), "out:\n{out}");
        assert!(out.contains("q = F10"), "out:\n{out}");
    }

    #[test]
    fn help_has_a_visible_title() {
        let out = draw_help_to_string(80, 24);
        assert!(
            out.contains("key bindings"),
            "expected modal title 'key bindings' in:\n{out}"
        );
    }

    #[test]
    fn centered_rect_fits_within_small_frame() {
        // 画面が小さい場合は clamp されて飛び出さないこと。
        let frame = Rect::new(0, 0, 20, 5);
        let center = centered_rect(frame);
        assert!(center.width <= frame.width);
        assert!(center.height <= frame.height);
        assert!(center.x + center.width <= frame.x + frame.width);
        assert!(center.y + center.height <= frame.y + frame.height);
    }

    #[test]
    fn centered_rect_picks_target_size_when_frame_is_large() {
        let frame = Rect::new(0, 0, 200, 50);
        let center = centered_rect(frame);
        assert_eq!(center.width, 74);
        assert_eq!(center.height, 16);
    }
}
