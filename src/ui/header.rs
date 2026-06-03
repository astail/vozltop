//! TUI 上段 7 行のヘッダ widget (issue #150)。
//!
//! ## レイアウト
//!
//! ```text
//! ╭─ ● api.prod · up 3d 14h ─────────────────────────────────────╮
//! │ Conn   active 42   reading 3   writing 5   waiting 4          │
//! │ ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄ │
//! │ RPS  ▉▉▉▉▉▉▉▉▉▉▉▍░░░░░  1234/s   │  req  1.58 M               │
//! │ IN   ▉▉▉▉▍░░░░░░░░░░░░  1.2 MB/s │  rx   1.50 GB              │
//! │ OUT  ▉▉▉▉▉▉▉▉▉▉▉▉▉▉▍░░  4.5 MB/s │  tx   5.20 GB              │
//! ╰───────────────────────────────────────────────────────────────╯
//! ```
//!
//! ## 設計判断 (issue #150)
//!
//! - **ヘッダ全体を rounded box で囲む** (mono は plain): ratatui の `Block` を
//!   1 つだけ使い、内側 5 行を `Layout::vertical` で分割する。
//! - **タイトル行に ● ステータスドット + host + uptime を集約**: 旧
//!   `render_status_banner` を廃止して状態をタイトルに統合。
//! - **bar は 1/8 サブセル smooth fill** (`█▉▊▋▌▍▎▏░`): ratatui `Gauge` 相当の
//!   なめらかさを `Paragraph` 上で実現。
//! - **Conn 行は絶対値表示のみ** (旧 Gauge 廃止): `rolling_max_active_conns` の
//!   分母腐り問題を bar ごと撤廃して根本解決。
//! - **RPS / IN/OUT bar の分母**: RPS は独立 60s sliding peak、IN/OUT は共通
//!   `peak_bw` (per-sample `max(in, out)` の 60s 内最大) で正規化。
//! - **右内カラム** (`│` 区切り) は累計値 (req / rx / tx)。Conn 行は full width。

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};
use ratatui::Frame;

use crate::state::{App, AppStatus};

/// ヘッダの行数 (固定 7 行 = 上枠 1 + 内側 5 + 下枠 1)。
pub const HEADER_HEIGHT: u16 = 7;

/// 1/8 サブセル smooth bar のグリフ (1/8 〜 7/8)。
/// 8/8 は `█` として独立扱い (full block で promote)。
const EIGHTHS: [char; 7] = ['▏', '▎', '▍', '▌', '▋', '▊', '▉'];

/// area を 1 つの rounded box で囲み、その内側に 5 行 (Conn / divider / RPS / IN / OUT)
/// を描画する。
///
/// `suppress_host_in_title` が `true` のとき、タイトル行から host 名を省く
/// (Workspace multi-host モードでは host 名が上段の host タブバーに既出のため、
/// 二重表示を避ける目的)。単一 host モードでは `false` を渡す。
pub fn render(f: &mut Frame<'_>, app: &App, area: Rect, suppress_host_in_title: bool) {
    if area.height < HEADER_HEIGHT || area.width < 4 {
        // 安全側: 極端な resize で何も描かない。panic はしない。
        return;
    }

    let border_type = if app.theme.mono {
        BorderType::Plain
    } else {
        BorderType::Rounded
    };
    let title_line = build_title_line(app, suppress_host_in_title);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(border_type)
        .border_style(app.theme.border)
        .title(title_line);
    let inner = block.inner(area);
    f.render_widget(block, area);

    if inner.height < 5 {
        return;
    }

    let rows = Layout::vertical([
        Constraint::Length(1), // Conn
        Constraint::Length(1), // divider
        Constraint::Length(1), // RPS
        Constraint::Length(1), // IN
        Constraint::Length(1), // OUT
    ])
    .split(inner);

    render_conn_row(f, app, rows[0]);
    render_divider(f, app, rows[1]);

    let peak_rps = app.history.peak_rps();
    let peak_bw = app.history.peak_bw();
    let (rps_now, bw_in_now, bw_out_now) = app
        .history
        .latest()
        .map(|s| s.derived.server_totals())
        .unwrap_or((0, 0, 0));

    let total_req = app.history.total_requests();
    let total_rx = app.history.total_in_bytes();
    let total_tx = app.history.total_out_bytes();

    render_bar_row(
        f,
        app,
        rows[2],
        "RPS",
        rps_now,
        peak_rps,
        &format!("{rps_now}/s"),
        "req",
        &format_count(total_req),
    );
    render_bar_row(
        f,
        app,
        rows[3],
        "IN",
        bw_in_now,
        peak_bw,
        &format_bps(bw_in_now),
        "rx",
        &format_bytes(total_rx),
    );
    render_bar_row(
        f,
        app,
        rows[4],
        "OUT",
        bw_out_now,
        peak_bw,
        &format_bps(bw_out_now),
        "tx",
        &format_bytes(total_tx),
    );
}

// ---------- タイトル行 ----------

/// タイトル文字列を `Line` として組み立てる。
///
/// `● {status_label} · {host} · up {uptime}` の形。
/// - Running: ラベル省略 (`● api.prod · up 3d 14h`)
/// - Connecting: `● Connecting`
/// - Stale: `● Stale (3) · api.prod · up 3d 14h`
/// - Disconnected: `● Disconnected (5) · api.prod · up 3d 14h`
///
/// `suppress_host` が `true` のとき host 名は出力しない (Workspace multi-host
/// モードで host タブバーと重複させないため)。
fn build_title_line(app: &App, suppress_host: bool) -> Line<'static> {
    let (dot_style, status_label): (Style, Option<String>) = match &app.status {
        AppStatus::Connecting => (app.theme.status_warn, Some("Connecting".to_string())),
        AppStatus::Running => (app.theme.status_ok, None),
        AppStatus::Stale { failures, .. } => {
            (app.theme.status_warn, Some(format!("Stale ({failures})")))
        }
        AppStatus::Disconnected { failures } => (
            app.theme.status_err,
            Some(format!("Disconnected ({failures})")),
        ),
    };

    let host = app
        .history
        .latest()
        .map(|s| s.status.host_name.clone())
        .unwrap_or_default();
    let uptime_ms = app.history.uptime_ms();

    let mut spans: Vec<Span<'static>> = Vec::with_capacity(8);
    // 左端に空白 1 文字を入れて枠線とドットを離す
    spans.push(Span::raw(" "));
    spans.push(Span::styled("●", dot_style));
    if let Some(label) = status_label {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(label, app.theme.title));
    }
    if !suppress_host && !host.is_empty() {
        spans.push(Span::raw(" · "));
        spans.push(Span::styled(host, app.theme.title));
    }
    if uptime_ms > 0 {
        spans.push(Span::raw(" · up "));
        spans.push(Span::styled(format_uptime(uptime_ms), app.theme.title));
    }
    spans.push(Span::raw(" "));
    Line::from(spans)
}

// ---------- 内側 5 行の描画 ----------

fn render_conn_row(f: &mut Frame<'_>, app: &App, area: Rect) {
    let conns = app.history.latest().map(|s| &s.status.connections);
    let active = conns.map(|c| c.active).unwrap_or(0);
    let reading = conns.map(|c| c.reading).unwrap_or(0);
    let writing = conns.map(|c| c.writing).unwrap_or(0);
    let waiting = conns.map(|c| c.waiting).unwrap_or(0);

    let line = Line::from(vec![
        Span::raw(" "),
        Span::styled("Conn", app.theme.header_label),
        Span::raw("   active "),
        Span::styled(active.to_string(), app.theme.header_value),
        Span::raw("  reading "),
        Span::styled(reading.to_string(), app.theme.header_value),
        Span::raw("  writing "),
        Span::styled(writing.to_string(), app.theme.header_value),
        Span::raw("  waiting "),
        Span::styled(waiting.to_string(), app.theme.header_value),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn render_divider(f: &mut Frame<'_>, app: &App, area: Rect) {
    if area.width == 0 {
        return;
    }
    let mut s = String::with_capacity(area.width as usize * 3);
    for _ in 0..area.width {
        s.push('┄');
    }
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(s, app.theme.separator))),
        area,
    );
}

/// 1 つの bar 行 (RPS / IN / OUT) を描画する。
///
/// 横レイアウト: `[Length(5), Fill(1), Length(12), Length(3), Length(20)]`
/// = ラベル / bar / 現在値 / `│` 区切り / 右内カラム (`req 1.58 M` 等)
///
/// `area.width` が狭くて右内カラムが入らない場合は右カラム + `│` を省略し、
/// bar 領域を Fill(1) に伸ばす (`area.width < 5 + 12 + 3 + RIGHT_BUDGET + 2`)。
#[allow(clippy::too_many_arguments)]
fn render_bar_row(
    f: &mut Frame<'_>,
    app: &App,
    area: Rect,
    label: &str,
    current: u64,
    peak: u64,
    value_text: &str,
    total_label: &str,
    total_value: &str,
) {
    const LABEL_WIDTH: u16 = 5;
    const VALUE_WIDTH: u16 = 12;
    const SEP_WIDTH: u16 = 3;
    const RIGHT_WIDTH: u16 = 20;
    const MIN_BAR: u16 = 8;
    let show_right_column =
        area.width >= LABEL_WIDTH + MIN_BAR + VALUE_WIDTH + SEP_WIDTH + RIGHT_WIDTH;

    let chunks = if show_right_column {
        Layout::horizontal([
            Constraint::Length(LABEL_WIDTH),
            Constraint::Fill(1),
            Constraint::Length(VALUE_WIDTH),
            Constraint::Length(SEP_WIDTH),
            Constraint::Length(RIGHT_WIDTH),
        ])
        .split(area)
    } else {
        Layout::horizontal([
            Constraint::Length(LABEL_WIDTH),
            Constraint::Fill(1),
            Constraint::Length(VALUE_WIDTH),
        ])
        .split(area)
    };

    // label
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw(" "),
            Span::styled(label.to_string(), app.theme.header_label),
        ])),
        chunks[0],
    );

    // bar
    let bar_area = chunks[1];
    let bar_width = bar_area.width;
    let (filled, empty) = smooth_bar(bar_width, current, peak);
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(filled, app.theme.bar_filled),
            Span::styled(empty, app.theme.bar_empty),
        ])),
        bar_area,
    );

    // value
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw(" "),
            Span::styled(value_text.to_string(), app.theme.header_value),
        ])),
        chunks[2],
    );

    if show_right_column {
        // separator `│`
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(" │ ", app.theme.separator))),
            chunks[3],
        );
        // right column: " {label}  {value}"
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::raw(" "),
                Span::styled(total_label.to_string(), app.theme.header_label),
                Span::raw("  "),
                Span::styled(total_value.to_string(), app.theme.header_value),
            ])),
            chunks[4],
        );
    }
}

// ---------- フォーマッタ ----------

/// `current` / `peak` を `width` cells の 1/8 サブセル smooth bar に整形する。
/// 戻り値は `(filled, empty)` の 2 文字列。連結すると `width` cells になる。
fn smooth_bar(width: u16, current: u64, peak: u64) -> (String, String) {
    if width == 0 {
        return (String::new(), String::new());
    }
    let ratio = if peak == 0 {
        0.0
    } else {
        (current as f64 / peak as f64).clamp(0.0, 1.0)
    };
    let total_eighths = width as u32 * 8;
    let filled_eighths = ((ratio * width as f64 * 8.0).round() as u32).min(total_eighths);
    let full = (filled_eighths / 8) as u16;
    let partial = (filled_eighths % 8) as usize;

    let mut filled = String::with_capacity((full as usize + 1) * 3);
    for _ in 0..full {
        filled.push('█');
    }
    let used_partial = if full < width && partial > 0 {
        filled.push(EIGHTHS[partial - 1]);
        1
    } else {
        0
    };
    let empty_cells = width.saturating_sub(full + used_partial);
    let mut empty = String::with_capacity(empty_cells as usize * 3);
    for _ in 0..empty_cells {
        empty.push('░');
    }
    (filled, empty)
}

/// uptime ミリ秒を `3d 14h` / `4h 22m` / `45m 12s` に整形する。
pub(crate) fn format_uptime(ms: u64) -> String {
    let total_secs = ms / 1000;
    let days = total_secs / 86_400;
    let hours = (total_secs % 86_400) / 3600;
    let minutes = (total_secs % 3600) / 60;
    let seconds = total_secs % 60;
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else {
        format!("{minutes}m {seconds}s")
    }
}

/// 累計件数 (req 用) を 10 進 SI prefix で整形する: `1.58 M` / `45.2 K`。
pub(crate) fn format_count(n: u64) -> String {
    if n < 1_000 {
        format!("{n}")
    } else if n < 1_000_000 {
        format!("{:.1} K", n as f64 / 1_000.0)
    } else if n < 1_000_000_000 {
        format!("{:.2} M", n as f64 / 1_000_000.0)
    } else if n < 1_000_000_000_000 {
        format!("{:.2} G", n as f64 / 1_000_000_000.0)
    } else {
        format!("{:.2} T", n as f64 / 1_000_000_000_000.0)
    }
}

/// 累計バイト数 (rx / tx 用) を 1024 進で整形する: `1.50 GB` / `512 MB`。
/// `format_bps` から `/s` を抜いた派生。
pub(crate) fn format_bytes(n: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    const TB: u64 = GB * 1024;
    if n < KB {
        format!("{n} B")
    } else if n < MB {
        format!("{:.1} KB", n as f64 / KB as f64)
    } else if n < GB {
        format!("{:.1} MB", n as f64 / MB as f64)
    } else if n < TB {
        format!("{:.2} GB", n as f64 / GB as f64)
    } else {
        format!("{:.2} TB", n as f64 / TB as f64)
    }
}

/// bytes-per-second を `1.2 MB/s` 形式に整形する (issue #27 から流用)。
pub(crate) fn format_bps(n: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    const TB: u64 = GB * 1024;
    if n < KB {
        format!("{n} B/s")
    } else if n < MB {
        format!("{:.1} KB/s", n as f64 / KB as f64)
    } else if n < GB {
        format!("{:.1} MB/s", n as f64 / MB as f64)
    } else if n < TB {
        format!("{:.1} GB/s", n as f64 / GB as f64)
    } else {
        format!("{:.1} TB/s", n as f64 / TB as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    use crate::model::VtsStatus;
    use crate::state::history::Snapshot;
    use crate::state::App;
    use crate::theme::Theme;

    fn snapshot_with_conns(
        now_msec: u64,
        active: u64,
        reading: u64,
        writing: u64,
        waiting: u64,
    ) -> Snapshot {
        let raw = serde_json::json!({
            "hostName": "host1", "nginxVersion": "1", "moduleVersion": "v",
            "loadMsec": 0u64, "nowMsec": now_msec,
            "connections": {
                "active": active, "reading": reading, "writing": writing,
                "waiting": waiting, "accepted": 0, "handled": 0, "requests": 0
            },
        });
        let status: VtsStatus = serde_json::from_value(raw).unwrap();
        Snapshot {
            at: std::time::Instant::now(),
            status,
            derived: Default::default(),
        }
    }

    fn draw(app: &App, w: u16, h: u16) -> String {
        draw_with(app, w, h, false)
    }

    fn draw_with(app: &App, w: u16, h: u16, suppress_host: bool) -> String {
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).expect("term");
        terminal
            .draw(|f| {
                let area = f.area();
                render(f, app, area, suppress_host);
            })
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

    // ---------- format_uptime ----------

    #[test]
    fn format_uptime_seconds() {
        assert_eq!(format_uptime(45 * 1000), "0m 45s");
    }
    #[test]
    fn format_uptime_minutes() {
        assert_eq!(format_uptime((45 * 60 + 12) * 1000), "45m 12s");
    }
    #[test]
    fn format_uptime_hours() {
        assert_eq!(format_uptime((4 * 3600 + 22 * 60) * 1000), "4h 22m");
    }
    #[test]
    fn format_uptime_days() {
        assert_eq!(format_uptime((3 * 86400 + 14 * 3600) * 1000), "3d 14h");
    }

    // ---------- format_count ----------

    #[test]
    fn format_count_below_1k() {
        assert_eq!(format_count(0), "0");
        assert_eq!(format_count(999), "999");
    }
    #[test]
    fn format_count_kilo() {
        assert_eq!(format_count(1_000), "1.0 K");
        assert_eq!(format_count(45_234), "45.2 K");
    }
    #[test]
    fn format_count_mega() {
        assert_eq!(format_count(1_580_000), "1.58 M");
    }
    #[test]
    fn format_count_giga() {
        assert_eq!(format_count(2_500_000_000), "2.50 G");
    }

    // ---------- format_bytes ----------

    #[test]
    fn format_bytes_b() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1023), "1023 B");
    }
    #[test]
    fn format_bytes_kb() {
        assert_eq!(format_bytes(1024), "1.0 KB");
    }
    #[test]
    fn format_bytes_mb() {
        assert_eq!(format_bytes(1024 * 1024), "1.0 MB");
    }
    #[test]
    fn format_bytes_gb() {
        assert_eq!(format_bytes(1024u64.pow(3)), "1.00 GB");
    }

    // ---------- format_bps ----------

    #[test]
    fn format_bps_bytes() {
        assert_eq!(format_bps(0), "0 B/s");
        assert_eq!(format_bps(1023), "1023 B/s");
    }
    #[test]
    fn format_bps_kilobytes() {
        assert_eq!(format_bps(1024), "1.0 KB/s");
    }
    #[test]
    fn format_bps_megabytes() {
        assert_eq!(format_bps(1024 * 1024), "1.0 MB/s");
    }

    // ---------- smooth_bar ----------

    #[test]
    fn smooth_bar_empty_when_peak_zero() {
        let (f, e) = smooth_bar(10, 100, 0);
        assert_eq!(f, "");
        assert_eq!(e, "░░░░░░░░░░");
    }

    #[test]
    fn smooth_bar_full_when_current_equals_peak() {
        let (f, e) = smooth_bar(10, 100, 100);
        assert_eq!(f, "██████████");
        assert_eq!(e, "");
    }

    #[test]
    fn smooth_bar_half_uses_full_blocks() {
        let (f, e) = smooth_bar(10, 50, 100);
        assert_eq!(f, "█████");
        assert_eq!(e, "░░░░░");
    }

    #[test]
    fn smooth_bar_partial_uses_subcell() {
        let (f, e) = smooth_bar(10, 45, 100);
        assert!(f.starts_with("████"));
        assert_eq!(f.chars().count(), 5, "4 full + 1 partial");
        assert_eq!(e.chars().count(), 5);
    }

    #[test]
    fn smooth_bar_width_zero_returns_empty() {
        let (f, e) = smooth_bar(0, 1, 1);
        assert_eq!(f, "");
        assert_eq!(e, "");
    }

    // ---------- header 描画 ----------

    #[test]
    fn header_renders_seven_rows_with_rounded_box() {
        let app = App::new();
        let out = draw(&app, 80, HEADER_HEIGHT);
        // 上下が rounded box の境界 (╭ ╮ ╰ ╯ または ─) で描画される
        assert!(out.contains('╭') || out.contains('┌'), "out:\n{out}");
        assert!(out.contains('╰') || out.contains('└'), "out:\n{out}");
    }

    #[test]
    fn header_renders_conn_breakdown() {
        let mut app = App::new();
        app.history.push(snapshot_with_conns(1000, 12, 3, 5, 4));
        let out = draw(&app, 80, HEADER_HEIGHT);
        assert!(out.contains("Conn"), "out:\n{out}");
        assert!(out.contains("active 12"), "out:\n{out}");
        assert!(out.contains("reading 3"), "out:\n{out}");
        assert!(out.contains("writing 5"), "out:\n{out}");
        assert!(out.contains("waiting 4"), "out:\n{out}");
    }

    #[test]
    fn header_renders_three_bar_labels() {
        let app = App::new();
        let out = draw(&app, 80, HEADER_HEIGHT);
        assert!(out.contains("RPS"), "out:\n{out}");
        assert!(out.contains("IN"), "out:\n{out}");
        assert!(out.contains("OUT"), "out:\n{out}");
    }

    #[test]
    fn header_renders_divider_with_dashed_glyph() {
        let app = App::new();
        let out = draw(&app, 80, HEADER_HEIGHT);
        assert!(out.contains('┄'), "divider should use ┄; out:\n{out}");
    }

    #[test]
    fn header_title_shows_host_when_running() {
        let mut app = App::new();
        app.history.push(snapshot_with_conns(1000, 1, 0, 1, 0));
        app.status = AppStatus::Running;
        let out = draw(&app, 80, HEADER_HEIGHT);
        assert!(out.contains("host1"), "out:\n{out}");
    }

    #[test]
    fn header_title_shows_status_label_when_stale() {
        let mut app = App::new();
        app.history.push(snapshot_with_conns(1000, 1, 0, 1, 0));
        app.status = AppStatus::Stale {
            last_ok: std::time::Instant::now(),
            failures: 3,
        };
        let out = draw(&app, 80, HEADER_HEIGHT);
        assert!(out.contains("Stale"), "out:\n{out}");
        assert!(out.contains("(3)"), "out:\n{out}");
    }

    #[test]
    fn header_title_shows_status_label_when_disconnected() {
        let mut app = App::new();
        app.status = AppStatus::Disconnected { failures: 5 };
        let out = draw(&app, 80, HEADER_HEIGHT);
        assert!(out.contains("Disconnected"), "out:\n{out}");
        assert!(out.contains("(5)"), "out:\n{out}");
    }

    #[test]
    fn header_renders_cumulative_labels_in_right_column() {
        let mut app = App::new();
        app.history.push(snapshot_with_conns(1000, 1, 0, 1, 0));
        let out = draw(&app, 80, HEADER_HEIGHT);
        assert!(out.contains(" req "), "out:\n{out}");
        assert!(out.contains(" rx "), "out:\n{out}");
        assert!(out.contains(" tx "), "out:\n{out}");
    }

    #[test]
    fn header_renders_pipe_separator_in_bar_rows() {
        let mut app = App::new();
        app.history.push(snapshot_with_conns(1000, 1, 0, 1, 0));
        let out = draw(&app, 80, HEADER_HEIGHT);
        assert!(out.contains('│'), "out:\n{out}");
    }

    #[test]
    fn header_renders_in_mono_theme_without_panic() {
        let mut app = App::with_theme(Theme::mono());
        app.history.push(snapshot_with_conns(1000, 1, 0, 1, 0));
        let out = draw(&app, 80, HEADER_HEIGHT);
        assert!(out.contains("Conn"), "out:\n{out}");
        assert!(out.contains("active 1"), "out:\n{out}");
    }

    #[test]
    fn header_does_not_panic_when_too_short() {
        let app = App::new();
        let backend = TestBackend::new(80, 3);
        let mut terminal = Terminal::new(backend).expect("term");
        terminal
            .draw(|f| {
                let area = f.area();
                render(f, &app, area, false);
            })
            .expect("draw");
    }

    #[test]
    fn multi_host_workspace_suppresses_host_in_title() {
        // issue #150 受入条件: Workspace multi-host モードではタイトル host 名が
        // 省略される (host タブバーと二重表示しないため)。
        // snapshot_with_conns は host_name="host1" を仕込むので、その文字列で判定する。
        let mut app = App::new();
        app.history.push(snapshot_with_conns(1000, 1, 0, 1, 0));

        let with_host = draw_with(&app, 80, HEADER_HEIGHT, false);
        assert!(
            with_host.contains("host1"),
            "single-host mode should include host name in title:\n{with_host}"
        );

        let without_host = draw_with(&app, 80, HEADER_HEIGHT, true);
        assert!(
            !without_host.contains("host1"),
            "multi-host mode should suppress host name in title:\n{without_host}"
        );
        // ● ドット + Conn 行などその他のヘッダ要素は残る
        assert!(without_host.contains('●'), "out:\n{without_host}");
        assert!(without_host.contains("Conn"), "out:\n{without_host}");
    }
}
