//! TUI 上段 3 行のヘッダ widget (issue #27)。
//!
//! ## レイアウト
//!
//! ```text
//! ┌────────────────────────────────────────────────────────────────┐
//! │ [Stale|Disconnected] consecutive failures: N                   │  <- 任意の状態 banner (1 行、ヘッダの「上」)
//! ├────────────────────────────────────────────────────────────────┤
//! │ Conn  [█████░░░░] 42/120   active 42  reading 3  writing 5  …  │  <- 行 1
//! │ RPS   ▁▂▃▅▇▇▆▄▂▁   1234/s   | 5xx 0.20%                        │  <- 行 2
//! │ in    ▁▂▃▅▇▆▄▂▁   1.2 MB/s   out  ▁▂▃▅▇▆▄▂▁   4.5 MB/s         │  <- 行 3
//! └────────────────────────────────────────────────────────────────┘
//! ```
//!
//! ## 設計判断
//!
//! - **Gauge auto-scale**: `connections.worker_connections` は VTS JSON に
//!   含まれないため、観測中の `rolling_max_active_conns` を分母に使う
//!   (CLAUDE.md 設計判断より)。`max = 0` のときは `max(1)` で 0 除算を回避。
//! - **5xx 比率は cumulative**: 「per-tick の 5xx ratio」を出すには `Responses`
//!   の差分が必要だが、`DerivedSnapshot.server` は per-zone ratio のみで
//!   counter を持たない。本 PR では「最新スナップショットの全 server zone
//!   累積カウンタを zone 横断で合算した 5xx 比率」を表示する。長時間
//!   稼動では値が固定化するが、運用上は十分な指標。per-tick 化は #21 で
//!   `DerivedSnapshot` に zone 横断 counter を持たせる必要があり、本 PR の
//!   scope 外。
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

/// ヘッダの行数 (固定 3 行)。
pub const HEADER_HEIGHT: u16 = 3;

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

/// `area` を 3 等分してヘッダを描画する。
///
/// `area.height < 3` の場合は下の行から欠ける (ratatui の `Layout` 既定挙動)。
/// `area.width` が極端に小さい場合も panic はしない (各 widget が clip)。
pub fn render(f: &mut Frame<'_>, app: &App, area: Rect) {
    let rows = Layout::vertical([
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
        render_bw_row(f, app, *r);
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

// ---------- 行 2: RPS + 5xx% ----------

fn render_rps_row(f: &mut Frame<'_>, app: &App, area: Rect) {
    let rps_data: Vec<u64> = app.history.rps_history().iter().copied().collect();
    let rps_now = rps_data.last().copied().unwrap_or(0);
    let pct_5xx = aggregate_5xx_pct(app);

    let [label, spark, rps_text, pct_text] = Layout::horizontal([
        Constraint::Length(5),  // "RPS "
        Constraint::Fill(1),    // sparkline (残り)
        Constraint::Length(12), // " 1234/s"
        Constraint::Length(14), // "  5xx N.NN%"
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
    let pct_str = match pct_5xx {
        Some(p) => format!("  5xx {p:.2}%"),
        None => "  5xx —".to_string(),
    };
    let pct_style = match pct_5xx {
        Some(p) if p > 0.0 => app.theme.status_err,
        _ => app.theme.header_value,
    };
    f.render_widget(Paragraph::new(Span::styled(pct_str, pct_style)), pct_text);
}

// ---------- 行 3: BW in / out ----------

fn render_bw_row(f: &mut Frame<'_>, app: &App, area: Rect) {
    let bw_in_data: Vec<u64> = app.history.bw_in_history().iter().copied().collect();
    let bw_out_data: Vec<u64> = app.history.bw_out_history().iter().copied().collect();
    let in_now = bw_in_data.last().copied().unwrap_or(0);
    let out_now = bw_out_data.last().copied().unwrap_or(0);

    // 横を半分割: in 側 / out 側
    let [in_half, out_half] =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(area);

    render_bw_half(f, app, in_half, "in  ", &bw_in_data, in_now);
    render_bw_half(f, app, out_half, "out ", &bw_out_data, out_now);
}

fn render_bw_half(
    f: &mut Frame<'_>,
    app: &App,
    area: Rect,
    label: &'static str,
    data: &[u64],
    now: u64,
) {
    let [label_area, spark_area, text_area] = Layout::horizontal([
        Constraint::Length(4),  // "in  " or "out "
        Constraint::Fill(1),    // sparkline
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

/// 最新スナップショットの全 server zone を集計して 5xx 比率 (%) を返す。
///
/// 分母 (合計レスポンス) が 0 のときは `None` (まだ何も観測していない)。
/// 戻り値は cumulative (= zone 横断の累計カウンタからの比率)。per-tick の
/// 5xx ratio は `DerivedSnapshot` の拡張が必要なため scope 外。
pub(crate) fn aggregate_5xx_pct(app: &App) -> Option<f64> {
    let snap = app.history.latest()?;
    let mut total_5xx: u64 = 0;
    let mut total_all: u64 = 0;
    for zone in snap.status.server_zones.values() {
        let r = &zone.responses;
        total_5xx = total_5xx.saturating_add(r.r5xx);
        let zone_total = r
            .r1xx
            .saturating_add(r.r2xx)
            .saturating_add(r.r3xx)
            .saturating_add(r.r4xx)
            .saturating_add(r.r5xx);
        total_all = total_all.saturating_add(zone_total);
    }
    if total_all == 0 {
        None
    } else {
        Some(100.0 * total_5xx as f64 / total_all as f64)
    }
}

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

    /// 5xx 集計用に server_zones 付きの snapshot を作る。
    fn snapshot_with_responses(r2xx: u64, r5xx: u64) -> Snapshot {
        let raw = serde_json::json!({
            "hostName": "h", "nginxVersion": "1", "moduleVersion": "v",
            "loadMsec": 0u64, "nowMsec": 1000,
            "connections": {
                "active": 0, "reading": 0, "writing": 0,
                "waiting": 0, "accepted": 0, "handled": 0, "requests": 0
            },
            "serverZones": {
                "default": {
                    "requestCounter": r2xx + r5xx,
                    "inBytes": 0, "outBytes": 0,
                    "responses": {
                        "1xx": 0, "2xx": r2xx, "3xx": 0, "4xx": 0, "5xx": r5xx,
                        "miss": 0, "bypass": 0, "expired": 0, "stale": 0,
                        "updating": 0, "revalidated": 0, "hit": 0, "scarce": 0,
                    },
                    "requestMsec": 0,
                    "requestMsecCounter": 0,
                }
            }
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
    fn header_renders_rps_label_and_5xx_when_no_data() {
        let app = App::new();
        let out = draw(&app, 80, HEADER_HEIGHT);
        assert!(out.contains("RPS"), "out:\n{out}");
        // データ無し時は 5xx は em-dash で表示
        assert!(out.contains("5xx"), "out:\n{out}");
    }

    #[test]
    fn header_renders_5xx_pct_from_aggregated_zones() {
        let mut app = App::new();
        // total = 100, 5xx = 5 → 5.00%
        app.history.push(snapshot_with_responses(95, 5));
        let pct = aggregate_5xx_pct(&app).expect("pct present when totals > 0");
        assert!((pct - 5.0).abs() < 1e-9, "expected 5.0, got {pct}");
    }

    #[test]
    fn aggregate_5xx_returns_none_when_no_responses() {
        let app = App::new();
        assert!(aggregate_5xx_pct(&app).is_none());
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
