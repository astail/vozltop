//! TUI 上段 4 行のヘッダ widget (issue #27 / #119)。
//!
//! ## レイアウト
//!
//! ```text
//! ┌────────────────────────────────────────────────────────────────┐
//! │ [Stale|Disconnected] consecutive failures: N                   │  <- 任意の状態 banner (1 行、ヘッダの「上」)
//! ├────────────────────────────────────────────────────────────────┤
//! │ Conn  [█████░░░░] 42/120   active 42  reading 3  writing 5  …  │  <- 行 1
//! │ RPS   ▁▂▃▅▇▇▆▄▂▁                                       1234/s  │  <- 行 2
//! │ in    ▁▂▃▅▇▆▄▂▁                                      1.2 MB/s  │  <- 行 3
//! │ out   ▁▂▃▅▇▆▄▂▁                                      4.5 MB/s  │  <- 行 4
//! └────────────────────────────────────────────────────────────────┘
//! ```
//!
//! ## 設計判断
//!
//! - **Gauge auto-scale**: `connections.worker_connections` は VTS JSON に
//!   含まれないため、観測中の `rolling_max_active_conns` を分母に使う
//!   (CLAUDE.md 設計判断より)。`max = 0` のときは `max(1)` で 0 除算を回避。
//! - **Sparkline data 取り出し**: `History` は `VecDeque<u64>` で履歴を持つ
//!   が ratatui 0.29 の `Sparkline::data` は `&[u64]` を要求するため、毎
//!   フレーム `Vec<u64>` を作って描画する。120 件 = 960 B / frame で
//!   許容コスト。
//! - **bytes/s 表示**: 本プロジェクトはまだ `humansize` を依存に加えていない
//!   (CLAUDE.md は将来的な使用を予定)。1024 進の最大 5 段 (B/KB/MB/GB/TB)
//!   で十分なので [`format_bps`] を inline で持つ。
//! - **theme 連携**: ラベル / 数値の Style は `Theme::header_label` /
//!   `Theme::header_value` を使用 (`Theme` 設計通り)。Gauge 自体の色は
//!   ratatui 既定の `gauge_style` のまま (mono でも視認できる block 文字)。

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Gauge, Paragraph, Sparkline};
use ratatui::Frame;

use crate::state::{App, AppStatus};

/// ヘッダの行数 (固定 4 行: Conn / RPS / in / out)。
pub const HEADER_HEIGHT: u16 = 4;

/// `Stale` / `Disconnected` 状態のときに、ヘッダの上に 1 行の status banner
/// を差し込むべきかを判定する。
pub fn show_status_banner(app: &App) -> bool {
    matches!(
        app.status,
        AppStatus::Stale { .. } | AppStatus::Disconnected { .. }
    )
}

/// `[Stale]` / `[Disconnected]` の 1 行 banner を描画する。
///
/// `app.status` が `Connecting` / `Running` の場合は何も描画しない
/// (呼び出し側で [`show_status_banner`] により area 自体を確保しない想定)。
pub fn render_status_banner(f: &mut Frame<'_>, app: &App, area: Rect) {
    let (label, style) = match &app.status {
        AppStatus::Stale { failures, .. } => (
            format!("[Stale]  consecutive failures: {failures}"),
            app.theme.status_warn,
        ),
        AppStatus::Disconnected { failures } => (
            format!("[Disconnected]  consecutive failures: {failures}"),
            app.theme.status_err,
        ),
        AppStatus::Connecting | AppStatus::Running => return,
    };
    f.render_widget(Paragraph::new(Line::from(Span::styled(label, style))), area);
}

/// `area` を 4 等分してヘッダを描画する。
///
/// `area.height < 4` の場合は下の行から欠ける (ratatui の `Layout` 既定挙動)。
/// `area.width` が極端に小さい場合も panic はしない (各 widget が clip)。
pub fn render(f: &mut Frame<'_>, app: &App, area: Rect) {
    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(area);

    if let Some(r) = rows.first() {
        render_conn_row(f, app, *r);
    }
    if let Some(r) = rows.get(1) {
        render_rps_row(f, app, *r);
    }
    if let Some(r) = rows.get(2) {
        let data: Vec<u64> = app.history.bw_in_history().iter().copied().collect();
        render_bw_row(f, app, *r, "in   ", &data);
    }
    if let Some(r) = rows.get(3) {
        let data: Vec<u64> = app.history.bw_out_history().iter().copied().collect();
        render_bw_row(f, app, *r, "out  ", &data);
    }
}

// ---------- 行 1: connections ----------

fn render_conn_row(f: &mut Frame<'_>, app: &App, area: Rect) {
    let conns = app.history.latest().map(|s| &s.status.connections);
    let active = conns.map(|c| c.active).unwrap_or(0);
    let reading = conns.map(|c| c.reading).unwrap_or(0);
    let writing = conns.map(|c| c.writing).unwrap_or(0);
    let waiting = conns.map(|c| c.waiting).unwrap_or(0);

    // rolling-max は最低 1 にして 0 除算を防ぐ。ratio は [0,1] にクランプ。
    let scale_max = app.history.rolling_max_active_conns().max(1);
    let ratio = ((active as f64) / (scale_max as f64)).clamp(0.0, 1.0);

    let [conn_label, gauge_area, detail_area] = Layout::horizontal([
        Constraint::Length(5),  // "Conn "
        Constraint::Length(20), // Gauge
        Constraint::Fill(1),
    ])
    .areas(area);

    f.render_widget(
        Paragraph::new(Span::styled("Conn ", app.theme.header_label)),
        conn_label,
    );

    let gauge_label = format!("{active}/{scale_max}");
    f.render_widget(Gauge::default().ratio(ratio).label(gauge_label), gauge_area);

    let detail = Line::from(vec![
        Span::raw("  active "),
        Span::styled(active.to_string(), app.theme.header_value),
        Span::raw("  reading "),
        Span::styled(reading.to_string(), app.theme.header_value),
        Span::raw("  writing "),
        Span::styled(writing.to_string(), app.theme.header_value),
        Span::raw("  waiting "),
        Span::styled(waiting.to_string(), app.theme.header_value),
    ]);
    f.render_widget(Paragraph::new(detail), detail_area);
}

// ---------- 行 2: RPS ----------

fn render_rps_row(f: &mut Frame<'_>, app: &App, area: Rect) {
    let rps_data: Vec<u64> = app.history.rps_history().iter().copied().collect();
    let rps_now = rps_data.last().copied().unwrap_or(0);

    let [label, spark, rps_text] = Layout::horizontal([
        Constraint::Length(5),  // "RPS "
        Constraint::Fill(1),    // sparkline (残り)
        Constraint::Length(12), // " 1234/s"
    ])
    .areas(area);

    f.render_widget(
        Paragraph::new(Span::styled("RPS  ", app.theme.header_label)),
        label,
    );
    f.render_widget(Sparkline::default().data(&rps_data), spark);
    f.render_widget(
        Paragraph::new(Span::styled(
            format!(" {rps_now}/s"),
            app.theme.header_value,
        )),
        rps_text,
    );
}

// ---------- 行 3 / 4: BW in / out (1 行ずつ独立) ----------

/// in / out それぞれを 1 行ぶん描画する。
///
/// 横レイアウトは RPS 行と揃え (`Length(5) + Fill(1) + Length(13)`)、ラベル列を
/// `Conn ` / `RPS  ` と同じ 5 桁にすることで縦のラベル位置が揃う。
fn render_bw_row(
    f: &mut Frame<'_>,
    app: &App,
    area: Rect,
    label: &'static str,
    data: &[u64],
) {
    let now = data.last().copied().unwrap_or(0);

    let [label_area, spark_area, text_area] = Layout::horizontal([
        Constraint::Length(5),  // "in   " or "out  "
        Constraint::Fill(1),    // sparkline (残り)
        Constraint::Length(13), // " 1234.5 MB/s"
    ])
    .areas(area);

    f.render_widget(
        Paragraph::new(Span::styled(label, app.theme.header_label)),
        label_area,
    );
    f.render_widget(Sparkline::default().data(data), spark_area);
    f.render_widget(
        Paragraph::new(Span::styled(
            format!(" {}", format_bps(now)),
            app.theme.header_value,
        )),
        text_area,
    );
}

// ---------- 集計 / フォーマット helper ----------

/// bytes-per-second を人間可読 ("1.2 MB/s" 等) に整形する。
///
/// 1024 進、小数 1 桁。範囲を `u64::MAX` まで持たせるため最大 TB/s まで扱う
/// (実用上は MB/s 〜 GB/s で十分)。
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
            "hostName": "h", "nginxVersion": "1", "moduleVersion": "v",
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
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).expect("term");
        terminal
            .draw(|f| {
                let area = f.area();
                render(f, app, area);
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

    // ---------- format_bps ----------

    #[test]
    fn format_bps_bytes() {
        assert_eq!(format_bps(0), "0 B/s");
        assert_eq!(format_bps(1023), "1023 B/s");
    }
    #[test]
    fn format_bps_kilobytes() {
        assert_eq!(format_bps(1024), "1.0 KB/s");
        assert_eq!(format_bps(1536), "1.5 KB/s");
    }
    #[test]
    fn format_bps_megabytes() {
        assert_eq!(format_bps(1024 * 1024), "1.0 MB/s");
        assert_eq!(format_bps(1024 * 1024 * 3 + 1024 * 512), "3.5 MB/s");
    }
    #[test]
    fn format_bps_gigabytes() {
        assert_eq!(format_bps(1024u64.pow(3)), "1.0 GB/s");
    }
    #[test]
    fn format_bps_terabytes() {
        assert_eq!(format_bps(1024u64.pow(4)), "1.0 TB/s");
    }

    // ---------- show_status_banner ----------

    #[test]
    fn status_banner_only_for_stale_or_disconnected() {
        let mut app = App::new();
        assert!(!show_status_banner(&app));
        app.status = AppStatus::Running;
        assert!(!show_status_banner(&app));
        app.status = AppStatus::Disconnected { failures: 3 };
        assert!(show_status_banner(&app));
        app.status = AppStatus::Stale {
            last_ok: std::time::Instant::now(),
            failures: 2,
        };
        assert!(show_status_banner(&app));
    }

    // ---------- header 描画 ----------

    #[test]
    fn header_renders_conn_gauge_and_counts() {
        let mut app = App::new();
        app.history.push(snapshot_with_conns(1000, 12, 3, 5, 4));
        let out = draw(&app, 80, HEADER_HEIGHT);
        assert!(out.contains("Conn"), "out:\n{out}");
        // 数値 "12" "3" "5" "4" がすべて含まれること
        assert!(out.contains("active 12"), "out:\n{out}");
        assert!(out.contains("reading 3"), "out:\n{out}");
        assert!(out.contains("writing 5"), "out:\n{out}");
        assert!(out.contains("waiting 4"), "out:\n{out}");
    }

    #[test]
    fn header_renders_rps_label_when_no_data() {
        let app = App::new();
        let out = draw(&app, 80, HEADER_HEIGHT);
        assert!(out.contains("RPS"), "out:\n{out}");
    }

    #[test]
    fn header_renders_bw_in_and_out_labels() {
        let app = App::new();
        let out = draw(&app, 80, HEADER_HEIGHT);
        // 行 3 に "in" / "out" のラベルが出ること
        assert!(out.contains("in"), "out:\n{out}");
        assert!(out.contains("out"), "out:\n{out}");
        // データ無しは "0 B/s" 表示
        assert!(out.contains("0 B/s"), "out:\n{out}");
    }

    #[test]
    fn status_banner_renders_stale_label() {
        let mut app = App::new();
        app.status = AppStatus::Stale {
            last_ok: std::time::Instant::now(),
            failures: 2,
        };
        // ヘッダ + banner で 4 行ぶん使う想定 (UI mod 側の合成は別テストで担保)
        let backend = TestBackend::new(80, 1);
        let mut terminal = Terminal::new(backend).expect("term");
        terminal
            .draw(|f| {
                let area = f.area();
                render_status_banner(f, &app, area);
            })
            .expect("draw");
        let buf = terminal.backend().buffer().clone();
        let mut s = String::new();
        for x in 0..buf.area.width {
            s.push_str(buf[(x, 0)].symbol());
        }
        assert!(s.contains("Stale"), "out: {s}");
        assert!(s.contains("failures: 2"), "out: {s}");
    }

    #[test]
    fn status_banner_renders_disconnected_label() {
        let mut app = App::new();
        app.status = AppStatus::Disconnected { failures: 5 };
        let backend = TestBackend::new(80, 1);
        let mut terminal = Terminal::new(backend).expect("term");
        terminal
            .draw(|f| {
                let area = f.area();
                render_status_banner(f, &app, area);
            })
            .expect("draw");
        let buf = terminal.backend().buffer().clone();
        let mut s = String::new();
        for x in 0..buf.area.width {
            s.push_str(buf[(x, 0)].symbol());
        }
        assert!(s.contains("Disconnected"), "out: {s}");
        assert!(s.contains("failures: 5"), "out: {s}");
    }

    #[test]
    fn fits_in_80x3_layout() {
        // 80x24 で崩れない (受け入れ条件)。最小高さで panic しないことも確認。
        let app = App::new();
        let _ = draw(&app, 80, HEADER_HEIGHT);
    }

    #[test]
    fn renders_in_mono_theme() {
        let mut app = App::with_theme(Theme::mono());
        app.history.push(snapshot_with_conns(1000, 1, 0, 1, 0));
        let out = draw(&app, 80, HEADER_HEIGHT);
        assert!(out.contains("Conn"), "out:\n{out}");
        assert!(out.contains("active 1"), "out:\n{out}");
    }
}
