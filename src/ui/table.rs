//! Server / Upstream / Cache タブのテーブル描画。
//!
//! Server タブ (issue #28) / Upstream タブ (issue #29) を本ファイルで実装する。
//! Cache タブは issue #30 で同じパターンに従って関数を追加する。
//!
//! ## 列構成 (Server タブ)
//!
//! | # | 列名 | 内容 | フォーマット規約 |
//! |---|------|------|------------------|
//! | 0 | ZONE | serverZones のキー | そのまま |
//! | 1 | RPS | (Δrequest_counter / dt) | < 100 は `12.3`、それ以上は整数 |
//! | 2 | 2xx% | window 内の status クラス比率 | `99.5%` / 不明は `—` |
//! | 3 | 4xx% | 同上 | 同上 |
//! | 4 | 5xx% | 同上 (非ゼロは theme.status_err で強調) | 同上 |
//! | 5 | p95 | request_buckets から線形補間 | `38ms` / `~45ms` / `>500ms` / `—` |
//! | 6 | IN/s | Δin_bytes / dt | `1.2 MB/s` (1024 進) |
//! | 7 | OUT/s | Δout_bytes / dt | 同上 |
//!
//! ## 列構成 (Upstream タブ)
//!
//! Server タブと同じ 0-7 列に加えて、8 列目 STATE を追加する。
//!
//! | # | 列名 | 内容 | フォーマット規約 |
//! |---|------|------|------------------|
//! | 0 | ZONE | `group/host:port` | `backend_api/127.0.0.1:9001` |
//! | 1-7 | 同上 | Server タブと同じ semantics | 同上 |
//! | 8 | STATE | `down` / `backup` / `up` | color: red / yellow / 通常、mono: `[D]` `[B]` `[U]` |
//!
//! Upstream の行は `upstream_zones: HashMap<group, Vec<UpstreamServer>>` を
//! 1 server = 1 行に展開する (CLAUDE.md「Upstream 行の粒度」)。group 集約は
//! Phase 2 に倒す。
//!
//! ## デフォルトソート
//!
//! issue #28 受け入れ条件「RPS 降順」をハードコードする。issue #31 で
//! `App::sort` を経由した動的ソートに置き換える予定。`SortState` への
//! 切り替えはレンダ層だけの変更で済むよう、本ファイルの sort 呼び出し箇所を
//! 1 ヶ所にまとめている (`sort_server_rows_default`)。
//!
//! ## p95 表示
//!
//! `state::percentile::PercentileResult` 4 variants をそのまま 4 表記に
//! マッピング: `Value` → `Nms` / `Average` → `~Nms` / `Overflow` → `>Nms` /
//! `NoData` → `—`。histogram 未設定 zone は `request_msec` (cumulative
//! 区間平均) を `Average` に詰めて返す。
//!
//! ## カーソル
//!
//! ratatui の `TableState` を毎フレーム new し、`app.cursor` を `select` で
//! 渡すことで `Table::row_highlight_style` を有効にする。スクロールも
//! `TableState` 任せ (cursor が body の表示範囲を外れたら自動でオフセット)。

use std::cmp::Ordering;

use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, Paragraph, Row, Table, TableState};
use ratatui::Frame;

use crate::model::{ServerZone, UpstreamServer, VtsStatus};
use crate::state::{percentile, App, PercentileResult, Tab};
use crate::ui::header::format_bps;

/// 8 列ヘッダ。`ui::table::tests::*` から覗くことを想定して pub const にしてある。
pub const SERVER_HEADERS: [&str; 8] = [
    "ZONE", "RPS", "2xx%", "4xx%", "5xx%", "p95", "IN/s", "OUT/s",
];

/// 9 列ヘッダ (Upstream タブ)。Server タブ + STATE。
pub const UPSTREAM_HEADERS: [&str; 9] = [
    "ZONE", "RPS", "2xx%", "4xx%", "5xx%", "p95", "IN/s", "OUT/s", "STATE",
];

/// Upstream server の状態。`down` / `backup` / `up` の 3 値。
///
/// VTS JSON では `down: bool` と `backup: bool` の独立フラグで持つが、表示・
/// ソートでは「down か backup か up か」の単一 enum で扱う方が分岐が単純。
/// nginx は 1 server に `down` と `backup` を同時設定可能だが、その場合は
/// `down` 優先 (= サービスから完全に外れている方が運用上重要) で表示する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UpstreamState {
    Up,
    Backup,
    Down,
}

impl UpstreamState {
    /// `UpstreamServer` の 2 つの bool フィールドから状態を判定する。
    fn from_server(s: &UpstreamServer) -> Self {
        if s.down {
            UpstreamState::Down
        } else if s.backup {
            UpstreamState::Backup
        } else {
            UpstreamState::Up
        }
    }

    /// STATE ソート時の優先順 (小さいほど先頭)。
    ///
    /// issue #29: `up` 先頭 → `backup` → `down` (ヘルス把握優先)。実際に
    /// STATE 列ソートを発火させるのは issue #31 (`App::sort` 経由の動的ソート)
    /// なので、本 PR では順序規約をテストで固定するだけ。#31 で使われ次第
    /// `allow(dead_code)` は外す。
    #[allow(dead_code)]
    fn sort_rank(&self) -> u8 {
        match self {
            UpstreamState::Up => 0,
            UpstreamState::Backup => 1,
            UpstreamState::Down => 2,
        }
    }
}

/// Server タブ 1 行ぶんの確定済み描画データ。
///
/// `String` / `Option<f64>` に展開済みで、レンダ層が文字列整形だけを行うように
/// 「数値計算 → ソート → フォーマット」を 3 段に分けている。テストは数値計算の
/// 段 (`build_server_rows`) に対して行うのが容易。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ServerRow {
    pub zone: String,
    pub rps: f64,
    pub r2xx_pct: Option<f64>,
    pub r4xx_pct: Option<f64>,
    pub r5xx_pct: Option<f64>,
    pub p95: PercentileResult,
    pub bw_in_per_sec: f64,
    pub bw_out_per_sec: f64,
}

/// `now` (最新 snapshot) と `prev` (前 snapshot)、`dt_secs` (経過秒) から
/// Server タブの行を組み立てる。
///
/// - `prev` が `None` の初 tick: RPS/BW/ratios は 0 / `None`。p95 は
///   `request_buckets` の有無で `NoData` / `Average` フォールバック。
/// - `dt_secs <= 0` (= nginx 再起動 / 同一 tick): 同上 (差分が取れないため)。
pub(crate) fn build_server_rows(
    now: &VtsStatus,
    prev: Option<&VtsStatus>,
    dt_secs: f64,
) -> Vec<ServerRow> {
    let dt_ok = dt_secs > 0.0;
    let mut rows: Vec<ServerRow> = now
        .server_zones
        .iter()
        .map(|(name, zone)| {
            let prev_zone = prev.and_then(|p| p.server_zones.get(name));
            let rps = per_sec(
                prev_zone.map(|z| z.request_counter),
                zone.request_counter,
                dt_secs,
            );
            let bw_in = per_sec(prev_zone.map(|z| z.in_bytes), zone.in_bytes, dt_secs);
            let bw_out = per_sec(prev_zone.map(|z| z.out_bytes), zone.out_bytes, dt_secs);
            let (r2, r4, r5) = if dt_ok {
                compute_ratios(prev_zone, zone)
            } else {
                (None, None, None)
            };
            let p95 = compute_p95(prev_zone, zone);
            ServerRow {
                zone: name.clone(),
                rps,
                r2xx_pct: r2,
                r4xx_pct: r4,
                r5xx_pct: r5,
                p95,
                bw_in_per_sec: bw_in,
                bw_out_per_sec: bw_out,
            }
        })
        .collect();

    sort_server_rows_default(&mut rows);
    rows
}

/// デフォルトソート (RPS 降順、tie は ZONE 名昇順)。
///
/// issue #28 受け入れ条件。issue #31 で `App::sort` 経由の動的ソートに
/// 差し替える予定。
pub(crate) fn sort_server_rows_default(rows: &mut [ServerRow]) {
    rows.sort_by(|a, b| {
        b.rps
            .partial_cmp(&a.rps)
            .unwrap_or(Ordering::Equal)
            .then_with(|| a.zone.cmp(&b.zone))
    });
}

/// Upstream タブ 1 行ぶんの確定済み描画データ。
///
/// ZONE 列は `"group/host:port"` 形式。状態は `state` フィールドで持ち、
/// 描画時に theme 経由で色付け、ソート時に `UpstreamState::sort_rank` を使う。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct UpstreamRow {
    pub zone: String,
    pub rps: f64,
    pub r2xx_pct: Option<f64>,
    pub r4xx_pct: Option<f64>,
    pub r5xx_pct: Option<f64>,
    pub p95: PercentileResult,
    pub bw_in_per_sec: f64,
    pub bw_out_per_sec: f64,
    pub state: UpstreamState,
}

/// `now` (最新 snapshot) と `prev` (前 snapshot)、`dt_secs` (経過秒) から
/// Upstream タブの行を組み立てる。
///
/// `upstream_zones: HashMap<group, Vec<UpstreamServer>>` を 1 server = 1 行に
/// 展開し、ZONE 列に `"group/host:port"` を入れる。`prev` が `None` の初 tick
/// は Server タブと同じく RPS/BW/ratios は 0 / `None`。p95 は histogram 有無で
/// `NoData` / `Average` フォールバック。
pub(crate) fn build_upstream_rows(
    now: &VtsStatus,
    prev: Option<&VtsStatus>,
    dt_secs: f64,
) -> Vec<UpstreamRow> {
    let dt_ok = dt_secs > 0.0;
    let mut rows: Vec<UpstreamRow> = Vec::new();
    for (group, servers) in &now.upstream_zones {
        let prev_servers = prev.and_then(|p| p.upstream_zones.get(group));
        for srv in servers {
            let prev_srv =
                prev_servers.and_then(|list| list.iter().find(|s| s.server == srv.server));
            let rps = per_sec(
                prev_srv.map(|s| s.request_counter),
                srv.request_counter,
                dt_secs,
            );
            let bw_in = per_sec(prev_srv.map(|s| s.in_bytes), srv.in_bytes, dt_secs);
            let bw_out = per_sec(prev_srv.map(|s| s.out_bytes), srv.out_bytes, dt_secs);
            let (r2, r4, r5) = if dt_ok {
                compute_ratios_upstream(prev_srv, srv)
            } else {
                (None, None, None)
            };
            let p95 = compute_p95_upstream(prev_srv, srv);
            rows.push(UpstreamRow {
                zone: format!("{group}/{}", srv.server),
                rps,
                r2xx_pct: r2,
                r4xx_pct: r4,
                r5xx_pct: r5,
                p95,
                bw_in_per_sec: bw_in,
                bw_out_per_sec: bw_out,
                state: UpstreamState::from_server(srv),
            });
        }
    }
    sort_upstream_rows_default(&mut rows);
    rows
}

/// デフォルトソート (RPS 降順、tie は ZONE 名昇順)。
///
/// Server タブ (`sort_server_rows_default`) と同じ規約。STATE 列ソートは
/// issue #31 で `App::sort` 経由の動的ソート (`UpstreamState::sort_rank`) に
/// 差し替える予定。
pub(crate) fn sort_upstream_rows_default(rows: &mut [UpstreamRow]) {
    rows.sort_by(|a, b| {
        b.rps
            .partial_cmp(&a.rps)
            .unwrap_or(Ordering::Equal)
            .then_with(|| a.zone.cmp(&b.zone))
    });
}

fn compute_ratios_upstream(
    prev: Option<&UpstreamServer>,
    now: &UpstreamServer,
) -> (Option<f64>, Option<f64>, Option<f64>) {
    let Some(prev_srv) = prev else {
        return (None, None, None);
    };
    let r = &now.responses;
    let p = &prev_srv.responses;
    let d1 = r.r1xx.saturating_sub(p.r1xx);
    let d2 = r.r2xx.saturating_sub(p.r2xx);
    let d3 = r.r3xx.saturating_sub(p.r3xx);
    let d4 = r.r4xx.saturating_sub(p.r4xx);
    let d5 = r.r5xx.saturating_sub(p.r5xx);
    let total = d1
        .saturating_add(d2)
        .saturating_add(d3)
        .saturating_add(d4)
        .saturating_add(d5);
    if total == 0 {
        return (None, None, None);
    }
    let scale = 100.0 / total as f64;
    (
        Some(d2 as f64 * scale),
        Some(d4 as f64 * scale),
        Some(d5 as f64 * scale),
    )
}

fn compute_p95_upstream(prev: Option<&UpstreamServer>, now: &UpstreamServer) -> PercentileResult {
    let has_buckets = now
        .request_buckets
        .as_ref()
        .is_some_and(|b| !b.msecs.is_empty());
    if !has_buckets {
        return percentile::average_fallback(now.request_msec);
    }
    let Some(prev_srv) = prev else {
        return PercentileResult::NoData;
    };
    match (&now.request_buckets, &prev_srv.request_buckets) {
        (Some(n), Some(p)) => percentile::percentile(p, n, 0.95),
        _ => PercentileResult::NoData,
    }
}

/// STATE 列の表示文字列。color モードは "down"/"backup"/"up"、mono モードは
/// `[D]`/`[B]`/`[U]` (NO_COLOR でも識別可能なよう英大文字)。
pub(crate) fn format_upstream_state(state: UpstreamState, mono: bool) -> &'static str {
    match (state, mono) {
        (UpstreamState::Up, false) => "up",
        (UpstreamState::Backup, false) => "backup",
        (UpstreamState::Down, false) => "down",
        (UpstreamState::Up, true) => "[U]",
        (UpstreamState::Backup, true) => "[B]",
        (UpstreamState::Down, true) => "[D]",
    }
}

fn per_sec(prev: Option<u64>, now: u64, dt_secs: f64) -> f64 {
    match prev {
        Some(p) if dt_secs > 0.0 => (now.saturating_sub(p)) as f64 / dt_secs,
        _ => 0.0,
    }
}

fn compute_ratios(
    prev: Option<&ServerZone>,
    now: &ServerZone,
) -> (Option<f64>, Option<f64>, Option<f64>) {
    let Some(prev_zone) = prev else {
        return (None, None, None);
    };
    let r = &now.responses;
    let p = &prev_zone.responses;
    let d1 = r.r1xx.saturating_sub(p.r1xx);
    let d2 = r.r2xx.saturating_sub(p.r2xx);
    let d3 = r.r3xx.saturating_sub(p.r3xx);
    let d4 = r.r4xx.saturating_sub(p.r4xx);
    let d5 = r.r5xx.saturating_sub(p.r5xx);
    let total = d1
        .saturating_add(d2)
        .saturating_add(d3)
        .saturating_add(d4)
        .saturating_add(d5);
    if total == 0 {
        return (None, None, None);
    }
    let scale = 100.0 / total as f64;
    (
        Some(d2 as f64 * scale),
        Some(d4 as f64 * scale),
        Some(d5 as f64 * scale),
    )
}

fn compute_p95(prev: Option<&ServerZone>, now: &ServerZone) -> PercentileResult {
    let has_buckets = now
        .request_buckets
        .as_ref()
        .is_some_and(|b| !b.msecs.is_empty());
    if !has_buckets {
        // histogram 未設定 zone: 累積 request_msec を fallback として返す。
        return percentile::average_fallback(now.request_msec);
    }
    let Some(prev_zone) = prev else {
        // 初 tick: histogram 差分が取れないので NoData
        return PercentileResult::NoData;
    };
    match (&now.request_buckets, &prev_zone.request_buckets) {
        (Some(n), Some(p)) => percentile::percentile(p, n, 0.95),
        _ => PercentileResult::NoData,
    }
}

// ---------- フォーマット (純粋関数。テストで挙動を固定) ----------

/// `PercentileResult` を p95 表示文字列に整形する。
pub(crate) fn format_p95(p: PercentileResult) -> String {
    match p {
        PercentileResult::Value(ms) => format!("{}ms", ms.round() as u64),
        PercentileResult::Overflow(max) => format!(">{max}ms"),
        PercentileResult::Average(ms) => format!("~{}ms", ms.round() as u64),
        PercentileResult::NoData => "—".to_string(),
    }
}

/// status クラス比率を `NN.N%` か `—` で整形する。
pub(crate) fn format_ratio(r: Option<f64>) -> String {
    match r {
        Some(v) => format!("{v:.1}%"),
        None => "—".to_string(),
    }
}

/// RPS を 100 以上は整数、100 未満は小数 1 桁で表示する。0 は `0`。
pub(crate) fn format_rps(rps: f64) -> String {
    if rps >= 100.0 {
        format!("{rps:.0}")
    } else if rps > 0.0 {
        format!("{rps:.1}")
    } else {
        "0".to_string()
    }
}

/// 現在の `active_tab` + `cursor` が指す zone 名を返す (Enter で詳細を開くため)。
///
/// Server タブは RPS 降順ソート後の行から `cursor` 位置を引く (render と同じ順)。
/// snapshot 未取得 / 行 0 件 / 未実装タブ (Upstream / Cache は #29 / #30) は
/// `None`。後続タブが実装されたら本関数に行解決を足す。
pub fn selected_zone(app: &App) -> Option<String> {
    if app.active_tab != Tab::Server {
        return None;
    }
    let now = app.history.latest()?;
    let prev = app.history.previous();
    let dt_secs = match prev {
        Some(p) => (now.status.now_msec.saturating_sub(p.status.now_msec) as f64) / 1000.0,
        None => 0.0,
    };
    let rows = build_server_rows(&now.status, prev.map(|p| &p.status), dt_secs);
    if rows.is_empty() {
        return None;
    }
    let idx = app.cursor.min(rows.len() - 1);
    Some(rows[idx].zone.clone())
}

// ---------- render ----------

/// 現在の `active_tab` に応じて Server / Upstream / Cache を描画する。
///
/// 副作用:
/// - `app.visible_rows` を「表示行数」で更新 (cursor 上限算出用)。
/// - `app.page_size` を「body 高さ」で更新 (PgUp/PgDn の移動量)。
///
/// Cache タブは issue #30 で実装するまでプレースホルダ。
pub fn render(f: &mut Frame<'_>, app: &App, area: Rect) {
    match app.active_tab {
        Tab::Server => render_server(f, app, area),
        Tab::Upstream => render_upstream(f, app, area),
        Tab::Cache => {
            let p = Paragraph::new(Line::from(Span::styled(
                "(Cache tab — implemented in #30)",
                Style::default().add_modifier(Modifier::DIM),
            )));
            f.render_widget(p, area);
            app.visible_rows.set(0);
            app.page_size
                .set(area.height.saturating_sub(1).max(1) as usize);
        }
    }
}

/// Server タブを `area` に描画する (旧 `render`)。
fn render_server(f: &mut Frame<'_>, app: &App, area: Rect) {
    let latest = app.history.latest();

    // Connecting 状態: snapshot がまだ無い → ヘッダだけ出してプレースホルダ。
    let Some(now) = latest else {
        let p = Paragraph::new(Line::from(Span::styled(
            "waiting for first VTS snapshot…",
            Style::default().add_modifier(Modifier::DIM),
        )));
        f.render_widget(p, area);
        app.visible_rows.set(0);
        app.page_size
            .set(area.height.saturating_sub(1).max(1) as usize);
        return;
    };

    let prev = app.history.previous();
    let dt_secs = match prev {
        Some(p) => {
            let dt_ms = now.status.now_msec.saturating_sub(p.status.now_msec) as f64;
            dt_ms / 1000.0
        }
        None => 0.0,
    };
    let rows = build_server_rows(&now.status, prev.map(|p| &p.status), dt_secs);

    let header =
        Row::new(SERVER_HEADERS.iter().map(|h| Cell::from(*h))).style(app.theme.table_header);

    let body_rows: Vec<Row> = rows
        .iter()
        .map(|r| {
            // 5xx% が非ゼロなら警告色で行頭から強調する。
            let row_style = if r.r5xx_pct.is_some_and(|p| p > 0.0) {
                app.theme.status_err
            } else {
                Style::default()
            };
            Row::new(vec![
                Cell::from(r.zone.clone()),
                Cell::from(format_rps(r.rps)),
                Cell::from(format_ratio(r.r2xx_pct)),
                Cell::from(format_ratio(r.r4xx_pct)),
                Cell::from(format_ratio(r.r5xx_pct)),
                Cell::from(format_p95(r.p95)),
                Cell::from(format_bps(r.bw_in_per_sec.round() as u64)),
                Cell::from(format_bps(r.bw_out_per_sec.round() as u64)),
            ])
            .style(row_style)
        })
        .collect();

    let widths = [
        Constraint::Min(10),    // ZONE (可変)
        Constraint::Length(8),  // RPS
        Constraint::Length(7),  // 2xx%
        Constraint::Length(7),  // 4xx%
        Constraint::Length(7),  // 5xx%
        Constraint::Length(8),  // p95
        Constraint::Length(10), // IN/s
        Constraint::Length(10), // OUT/s
    ];
    let row_count = body_rows.len();

    let table = Table::new(body_rows, widths)
        .header(header)
        .row_highlight_style(app.theme.row_selected);

    // cursor を TableState に渡し、ratatui の組込みハイライト + 自動スクロールに任せる。
    let mut state = TableState::default();
    // cursor が visible_rows を超えていたら row_count - 1 に詰める (resize / zone 消失対策)。
    let clamped = if row_count == 0 {
        None
    } else {
        Some(app.cursor.min(row_count - 1))
    };
    state.select(clamped);

    f.render_stateful_widget(table, area, &mut state);

    // cursor 範囲計算用の値を書き戻す (interior mutability)。
    app.visible_rows.set(row_count);
    // 「ページ」= body 部の高さ。area.height からヘッダ 1 行を引く (最低 1)。
    app.page_size
        .set(area.height.saturating_sub(1).max(1) as usize);
}

/// Upstream タブを `area` に描画する。
///
/// 副作用は `render_server` と同じ (`app.visible_rows` / `app.page_size`)。
/// STATE 列は theme に応じて `up`/`backup`/`down` または `[U]`/`[B]`/`[D]` を
/// 出し、`backup` / `down` は theme の警告色 / エラー色で強調する。
fn render_upstream(f: &mut Frame<'_>, app: &App, area: Rect) {
    let latest = app.history.latest();

    let Some(now) = latest else {
        let p = Paragraph::new(Line::from(Span::styled(
            "waiting for first VTS snapshot…",
            Style::default().add_modifier(Modifier::DIM),
        )));
        f.render_widget(p, area);
        app.visible_rows.set(0);
        app.page_size
            .set(area.height.saturating_sub(1).max(1) as usize);
        return;
    };

    let prev = app.history.previous();
    let dt_secs = match prev {
        Some(p) => {
            let dt_ms = now.status.now_msec.saturating_sub(p.status.now_msec) as f64;
            dt_ms / 1000.0
        }
        None => 0.0,
    };
    let rows = build_upstream_rows(&now.status, prev.map(|p| &p.status), dt_secs);

    let header =
        Row::new(UPSTREAM_HEADERS.iter().map(|h| Cell::from(*h))).style(app.theme.table_header);

    let mono = app.theme.mono;
    let body_rows: Vec<Row> = rows
        .iter()
        .map(|r| {
            // 行全体の強調: 5xx% が非ゼロ ならエラー色。STATE 自体の強調は
            // STATE セル単位で行う (down/backup の行の他の列を赤一色にすると
            // 数値の読み取り性が落ちる)。
            let row_style = if r.r5xx_pct.is_some_and(|p| p > 0.0) {
                app.theme.status_err
            } else {
                Style::default()
            };
            let state_style = match r.state {
                UpstreamState::Down => app.theme.status_err,
                UpstreamState::Backup => app.theme.status_warn,
                UpstreamState::Up => Style::default(),
            };
            Row::new(vec![
                Cell::from(r.zone.clone()),
                Cell::from(format_rps(r.rps)),
                Cell::from(format_ratio(r.r2xx_pct)),
                Cell::from(format_ratio(r.r4xx_pct)),
                Cell::from(format_ratio(r.r5xx_pct)),
                Cell::from(format_p95(r.p95)),
                Cell::from(format_bps(r.bw_in_per_sec.round() as u64)),
                Cell::from(format_bps(r.bw_out_per_sec.round() as u64)),
                Cell::from(Span::styled(
                    format_upstream_state(r.state, mono),
                    state_style,
                )),
            ])
            .style(row_style)
        })
        .collect();

    let widths = [
        Constraint::Min(10),    // ZONE (可変、"group/host:port" を表示)
        Constraint::Length(8),  // RPS
        Constraint::Length(7),  // 2xx%
        Constraint::Length(7),  // 4xx%
        Constraint::Length(7),  // 5xx%
        Constraint::Length(8),  // p95
        Constraint::Length(10), // IN/s
        Constraint::Length(10), // OUT/s
        Constraint::Length(7),  // STATE ("backup" = 6 字 + 余白 1)
    ];
    let row_count = body_rows.len();

    let table = Table::new(body_rows, widths)
        .header(header)
        .row_highlight_style(app.theme.row_selected);

    let mut state = TableState::default();
    let clamped = if row_count == 0 {
        None
    } else {
        Some(app.cursor.min(row_count - 1))
    };
    state.select(clamped);

    f.render_stateful_widget(table, area, &mut state);

    app.visible_rows.set(row_count);
    app.page_size
        .set(area.height.saturating_sub(1).max(1) as usize);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    use crate::model::VtsStatus;
    use crate::state::App;

    /// テスト用 zone 定義 (clippy::type-complexity 回避のため type alias)。
    /// `(name, request_counter, in_bytes, out_bytes, request_msec,
    /// (1xx, 2xx, 3xx, 4xx, 5xx), Some((msecs, counters)) or None)`。
    type ZoneSpec<'a> = (
        &'a str,
        u64,
        u64,
        u64,
        u64,
        (u64, u64, u64, u64, u64),
        Option<(Vec<u64>, Vec<u64>)>,
    );

    /// `serverZones` 入りの最小 VtsStatus を作る。
    fn status_with_zones(now_msec: u64, zones: &[ZoneSpec<'_>]) -> VtsStatus {
        let server_zones: serde_json::Map<String, serde_json::Value> = zones
            .iter()
            .map(|(name, rc, ib, ob, rms, (r1, r2, r3, r4, r5), buckets)| {
                let buckets_json = match buckets {
                    Some((msecs, counters)) => serde_json::json!({
                        "msecs": msecs,
                        "counters": counters,
                    }),
                    None => serde_json::json!({ "msecs": [], "counters": [] }),
                };
                let v = serde_json::json!({
                    "requestCounter": rc,
                    "inBytes": ib,
                    "outBytes": ob,
                    "responses": {
                        "1xx": r1, "2xx": r2, "3xx": r3, "4xx": r4, "5xx": r5,
                        "miss": 0, "bypass": 0, "expired": 0, "stale": 0,
                        "updating": 0, "revalidated": 0, "hit": 0, "scarce": 0,
                    },
                    "requestMsec": rms,
                    "requestMsecCounter": 0,
                    "requestBuckets": buckets_json,
                });
                ((*name).to_string(), v)
            })
            .collect();
        let raw = serde_json::json!({
            "hostName": "h", "nginxVersion": "1", "moduleVersion": "v",
            "loadMsec": 0u64, "nowMsec": now_msec,
            "connections": {
                "active": 0, "reading": 0, "writing": 0,
                "waiting": 0, "accepted": 0, "handled": 0, "requests": 0
            },
            "serverZones": serde_json::Value::Object(server_zones),
        });
        serde_json::from_value(raw).unwrap()
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

    // ---------- format_* ----------

    #[test]
    fn format_p95_value_rounds_to_integer_ms() {
        assert_eq!(format_p95(PercentileResult::Value(37.6)), "38ms");
        assert_eq!(format_p95(PercentileResult::Value(0.0)), "0ms");
    }
    #[test]
    fn format_p95_average_prefixes_tilde() {
        assert_eq!(format_p95(PercentileResult::Average(45.0)), "~45ms");
    }
    #[test]
    fn format_p95_overflow_prefixes_gt() {
        assert_eq!(format_p95(PercentileResult::Overflow(500)), ">500ms");
    }
    #[test]
    fn format_p95_nodata_is_emdash() {
        assert_eq!(format_p95(PercentileResult::NoData), "—");
    }

    #[test]
    fn format_ratio_one_decimal() {
        assert_eq!(format_ratio(Some(99.9499)), "99.9%");
        assert_eq!(format_ratio(Some(0.0)), "0.0%");
        assert_eq!(format_ratio(None), "—");
    }

    #[test]
    fn format_rps_branches_on_magnitude() {
        assert_eq!(format_rps(0.0), "0");
        assert_eq!(format_rps(12.34), "12.3");
        // Rust の `{:.0}` は banker's rounding (round-half-to-even) なので
        // 1234.5 → "1234"。境界値 1234.6 で「整数 + 切り上げ」を確認。
        assert_eq!(format_rps(1234.6), "1235");
        // 境界 (100): 100.0 はちょうど整数扱いに入る
        assert_eq!(format_rps(100.0), "100");
    }

    // ---------- build_server_rows / sort ----------

    #[test]
    fn build_rows_with_no_prev_returns_zero_rps_and_nodata_or_average_p95() {
        // histogram あり zone: 初 tick なので NoData
        let s = status_with_zones(
            1000,
            &[(
                "zone-a",
                0,
                0,
                0,
                0,
                (0, 0, 0, 0, 0),
                Some((vec![10], vec![0])),
            )],
        );
        let rows = build_server_rows(&s, None, 0.0);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].rps, 0.0);
        assert!(matches!(rows[0].p95, PercentileResult::NoData));

        // histogram 無し zone: 初 tick でも average_fallback で値を返す
        let s2 = status_with_zones(1000, &[("zone-b", 0, 0, 0, 42, (0, 0, 0, 0, 0), None)]);
        let rows2 = build_server_rows(&s2, None, 0.0);
        assert!(matches!(rows2[0].p95, PercentileResult::Average(v) if (v - 42.0).abs() < 1e-9));
    }

    #[test]
    fn build_rows_computes_rps_and_ratios_from_diff() {
        let prev = status_with_zones(1000, &[("z", 100, 0, 0, 0, (0, 80, 0, 10, 10), None)]);
        let now = status_with_zones(
            2000,
            &[("z", 200, 1024, 4096, 0, (0, 180, 0, 10, 10), None)],
        );
        let rows = build_server_rows(&now, Some(&prev), 1.0);
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert!((r.rps - 100.0).abs() < 1e-9, "rps = {}", r.rps);
        // 差分: 1xx=0, 2xx=100, 3xx=0, 4xx=0, 5xx=0, total=100 → 2xx=100%, 4xx=0%, 5xx=0%
        assert!((r.r2xx_pct.unwrap() - 100.0).abs() < 1e-9);
        assert_eq!(r.r4xx_pct, Some(0.0));
        assert_eq!(r.r5xx_pct, Some(0.0));
        assert!((r.bw_in_per_sec - 1024.0).abs() < 1e-9);
        assert!((r.bw_out_per_sec - 4096.0).abs() < 1e-9);
    }

    #[test]
    fn default_sort_is_rps_desc_with_zone_tiebreak() {
        let now = status_with_zones(
            2000,
            &[
                ("alpha", 100, 0, 0, 0, (0, 90, 0, 5, 5), None),
                ("beta", 1000, 0, 0, 0, (0, 1000, 0, 0, 0), None),
                ("gamma", 100, 0, 0, 0, (0, 90, 0, 5, 5), None),
            ],
        );
        let prev = status_with_zones(
            1000,
            &[
                ("alpha", 50, 0, 0, 0, (0, 45, 0, 3, 2), None),
                ("beta", 500, 0, 0, 0, (0, 500, 0, 0, 0), None),
                ("gamma", 50, 0, 0, 0, (0, 45, 0, 3, 2), None),
            ],
        );
        let rows = build_server_rows(&now, Some(&prev), 1.0);
        // 期待順: beta (500 rps) → alpha (50 rps, tie で alphabetical) → gamma (50 rps)
        assert_eq!(rows[0].zone, "beta");
        assert_eq!(rows[1].zone, "alpha");
        assert_eq!(rows[2].zone, "gamma");
    }

    #[test]
    fn p95_uses_histogram_when_present() {
        // window 内に 50ms 以下が 100 件入る histogram。p95 は (10, 50] 区間で線形補間
        let prev = status_with_zones(
            1000,
            &[(
                "h",
                0,
                0,
                0,
                0,
                (0, 0, 0, 0, 0),
                Some((vec![10, 50, 100], vec![0, 0, 0])),
            )],
        );
        let now = status_with_zones(
            2000,
            &[(
                "h",
                100,
                0,
                0,
                0,
                (0, 100, 0, 0, 0),
                Some((vec![10, 50, 100], vec![0, 100, 100])),
            )],
        );

        let rows = build_server_rows(&now, Some(&prev), 1.0);
        match rows[0].p95 {
            PercentileResult::Value(ms) => {
                // 50ms 以下に 100 件、95% = 95 件目はその bucket 内に入る。lower=10, upper=50。
                assert!((10.0..=50.0).contains(&ms), "p95 = {ms}");
            }
            other => panic!("expected Value, got {other:?}"),
        }
    }

    // ---------- render ----------

    #[test]
    fn render_shows_header_row_and_zones() {
        let mut app = App::new();
        app.on_fetch_ok(status_with_zones(
            2000,
            &[("alpha", 100, 0, 0, 50, (0, 100, 0, 0, 0), None)],
        ));
        let out = draw(&app, 80, 5);
        for h in &SERVER_HEADERS {
            assert!(out.contains(h), "header {h} missing in:\n{out}");
        }
        assert!(out.contains("alpha"), "out:\n{out}");
        // histogram 無し zone は ~Nms (Average) 表示
        assert!(out.contains("~50ms"), "out:\n{out}");
    }

    #[test]
    fn render_with_no_snapshot_shows_placeholder() {
        let app = App::new();
        let out = draw(&app, 80, 3);
        assert!(
            out.contains("waiting for first VTS snapshot"),
            "out:\n{out}"
        );
    }

    #[test]
    fn render_updates_visible_rows_and_page_size() {
        let mut app = App::new();
        app.on_fetch_ok(status_with_zones(
            2000,
            &[
                ("a", 0, 0, 0, 0, (0, 0, 0, 0, 0), None),
                ("b", 0, 0, 0, 0, (0, 0, 0, 0, 0), None),
                ("c", 0, 0, 0, 0, (0, 0, 0, 0, 0), None),
            ],
        ));
        let _ = draw(&app, 80, 7); // 1 row header + 3 body + 余白
        assert_eq!(app.visible_rows.get(), 3);
        // body height = area.height - 1 = 6
        assert_eq!(app.page_size.get(), 6);
    }

    #[test]
    fn fits_in_80x24() {
        // 受け入れ条件: 80x24 で崩れない
        let mut app = App::new();
        app.on_fetch_ok(status_with_zones(
            2000,
            &[("alpha", 0, 0, 0, 0, (0, 0, 0, 0, 0), None)],
        ));
        let _ = draw(&app, 80, 24);
    }

    #[test]
    fn fits_in_120x40() {
        let mut app = App::new();
        app.on_fetch_ok(status_with_zones(
            2000,
            &[("alpha", 0, 0, 0, 0, (0, 0, 0, 0, 0), None)],
        ));
        let _ = draw(&app, 120, 40);
    }

    // ========== Upstream タブ (issue #29) ==========

    /// upstream server 1 台ぶんのテスト定義 (clippy::type-complexity 回避)。
    /// `(group, server, request_counter, in_bytes, out_bytes, request_msec,
    /// (1xx,2xx,3xx,4xx,5xx), Some((msecs,counters)) or None, backup, down)`。
    type UpstreamSpec<'a> = (
        &'a str,
        &'a str,
        u64,
        u64,
        u64,
        u64,
        (u64, u64, u64, u64, u64),
        Option<(Vec<u64>, Vec<u64>)>,
        bool,
        bool,
    );

    /// `upstreamZones` 入りの VtsStatus を作る。server は group ごとに集約される
    /// (キー順は BTreeMap で安定)。
    fn status_with_upstreams(now_msec: u64, servers: &[UpstreamSpec<'_>]) -> VtsStatus {
        use std::collections::BTreeMap;
        let mut groups: BTreeMap<String, Vec<serde_json::Value>> = BTreeMap::new();
        for (group, srv, rc, ib, ob, rms, (r1, r2, r3, r4, r5), buckets, backup, down) in servers {
            let buckets_json = match buckets {
                Some((msecs, counters)) => serde_json::json!({
                    "msecs": msecs,
                    "counters": counters,
                }),
                None => serde_json::json!({ "msecs": [], "counters": [] }),
            };
            let v = serde_json::json!({
                "server": srv,
                "requestCounter": rc,
                "inBytes": ib,
                "outBytes": ob,
                "responses": {
                    "1xx": r1, "2xx": r2, "3xx": r3, "4xx": r4, "5xx": r5,
                    "miss": 0, "bypass": 0, "expired": 0, "stale": 0,
                    "updating": 0, "revalidated": 0, "hit": 0, "scarce": 0,
                },
                "requestMsec": rms,
                "requestMsecCounter": 0,
                "requestBuckets": buckets_json,
                "responseMsec": 0,
                "responseMsecCounter": 0,
                "responseBuckets": { "msecs": [], "counters": [] },
                "weight": 1, "maxFails": 1, "failTimeout": 10,
                "backup": backup, "down": down,
            });
            groups.entry((*group).to_string()).or_default().push(v);
        }
        let upstream_zones: serde_json::Map<String, serde_json::Value> = groups
            .into_iter()
            .map(|(k, arr)| (k, serde_json::Value::Array(arr)))
            .collect();
        let raw = serde_json::json!({
            "hostName": "h", "nginxVersion": "1", "moduleVersion": "v",
            "loadMsec": 0u64, "nowMsec": now_msec,
            "connections": {
                "active": 0, "reading": 0, "writing": 0,
                "waiting": 0, "accepted": 0, "handled": 0, "requests": 0
            },
            "upstreamZones": serde_json::Value::Object(upstream_zones),
        });
        serde_json::from_value(raw).unwrap()
    }

    /// `active_tab = Upstream` に切り替えて描画した結果を文字列で返す。
    fn draw_upstream(app: &mut App, w: u16, h: u16) -> String {
        app.active_tab = Tab::Upstream;
        draw(app, w, h)
    }

    // ---------- UpstreamState ----------

    #[test]
    fn upstream_state_from_server_classifies_up_backup_down() {
        let mk = |backup: bool, down: bool| {
            let s = status_with_upstreams(
                1000,
                &[("g", "h:1", 0, 0, 0, 0, (0, 0, 0, 0, 0), None, backup, down)],
            );
            UpstreamState::from_server(&s.upstream_zones["g"][0])
        };
        assert_eq!(mk(false, false), UpstreamState::Up);
        assert_eq!(mk(true, false), UpstreamState::Backup);
        assert_eq!(mk(false, true), UpstreamState::Down);
        // down と backup を同時設定したら down 優先 (サービスから外れている方が重要)。
        assert_eq!(mk(true, true), UpstreamState::Down);
    }

    #[test]
    fn upstream_state_sort_rank_orders_up_then_backup_then_down() {
        // issue #29 受け入れ条件: STATE ソート優先順は up → backup → down。
        assert!(UpstreamState::Up.sort_rank() < UpstreamState::Backup.sort_rank());
        assert!(UpstreamState::Backup.sort_rank() < UpstreamState::Down.sort_rank());
    }

    #[test]
    fn format_upstream_state_color_and_mono() {
        assert_eq!(format_upstream_state(UpstreamState::Up, false), "up");
        assert_eq!(
            format_upstream_state(UpstreamState::Backup, false),
            "backup"
        );
        assert_eq!(format_upstream_state(UpstreamState::Down, false), "down");
        assert_eq!(format_upstream_state(UpstreamState::Up, true), "[U]");
        assert_eq!(format_upstream_state(UpstreamState::Backup, true), "[B]");
        assert_eq!(format_upstream_state(UpstreamState::Down, true), "[D]");
    }

    // ---------- build_upstream_rows ----------

    #[test]
    fn build_upstream_rows_zone_is_group_slash_server() {
        let s = status_with_upstreams(
            1000,
            &[
                (
                    "backend_api",
                    "127.0.0.1:9001",
                    0,
                    0,
                    0,
                    0,
                    (0, 0, 0, 0, 0),
                    None,
                    false,
                    false,
                ),
                (
                    "backend_api",
                    "127.0.0.1:9002",
                    0,
                    0,
                    0,
                    0,
                    (0, 0, 0, 0, 0),
                    None,
                    false,
                    false,
                ),
            ],
        );
        let rows = build_upstream_rows(&s, None, 0.0);
        let zones: Vec<&str> = rows.iter().map(|r| r.zone.as_str()).collect();
        assert!(
            zones.contains(&"backend_api/127.0.0.1:9001"),
            "zones: {zones:?}"
        );
        assert!(
            zones.contains(&"backend_api/127.0.0.1:9002"),
            "zones: {zones:?}"
        );
    }

    #[test]
    fn build_upstream_rows_no_prev_returns_zero_rps() {
        let s = status_with_upstreams(
            1000,
            &[("g", "h:1", 0, 0, 0, 42, (0, 0, 0, 0, 0), None, false, false)],
        );
        let rows = build_upstream_rows(&s, None, 0.0);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].rps, 0.0);
        // histogram 無し server は average_fallback で値を返す
        assert!(matches!(rows[0].p95, PercentileResult::Average(v) if (v - 42.0).abs() < 1e-9));
    }

    #[test]
    fn build_upstream_rows_computes_rps_and_ratios_from_diff() {
        let prev = status_with_upstreams(
            1000,
            &[(
                "g",
                "h:1",
                100,
                0,
                0,
                0,
                (0, 80, 0, 10, 10),
                None,
                false,
                false,
            )],
        );
        let now = status_with_upstreams(
            2000,
            &[(
                "g",
                "h:1",
                200,
                1024,
                4096,
                0,
                (0, 180, 0, 10, 10),
                None,
                false,
                false,
            )],
        );
        let rows = build_upstream_rows(&now, Some(&prev), 1.0);
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert!((r.rps - 100.0).abs() < 1e-9, "rps = {}", r.rps);
        // 差分: 2xx=100, total=100 → 2xx=100%
        assert!((r.r2xx_pct.unwrap() - 100.0).abs() < 1e-9);
        assert_eq!(r.r4xx_pct, Some(0.0));
        assert_eq!(r.r5xx_pct, Some(0.0));
        assert!((r.bw_in_per_sec - 1024.0).abs() < 1e-9);
        assert!((r.bw_out_per_sec - 4096.0).abs() < 1e-9);
        assert_eq!(r.state, UpstreamState::Up);
    }

    #[test]
    fn build_upstream_rows_default_sort_is_rps_desc_with_zone_tiebreak() {
        let now = status_with_upstreams(
            2000,
            &[
                (
                    "g",
                    "alpha:1",
                    100,
                    0,
                    0,
                    0,
                    (0, 90, 0, 5, 5),
                    None,
                    false,
                    false,
                ),
                (
                    "g",
                    "beta:1",
                    1000,
                    0,
                    0,
                    0,
                    (0, 1000, 0, 0, 0),
                    None,
                    false,
                    false,
                ),
                (
                    "g",
                    "gamma:1",
                    100,
                    0,
                    0,
                    0,
                    (0, 90, 0, 5, 5),
                    None,
                    false,
                    false,
                ),
            ],
        );
        let prev = status_with_upstreams(
            1000,
            &[
                (
                    "g",
                    "alpha:1",
                    50,
                    0,
                    0,
                    0,
                    (0, 45, 0, 3, 2),
                    None,
                    false,
                    false,
                ),
                (
                    "g",
                    "beta:1",
                    500,
                    0,
                    0,
                    0,
                    (0, 500, 0, 0, 0),
                    None,
                    false,
                    false,
                ),
                (
                    "g",
                    "gamma:1",
                    50,
                    0,
                    0,
                    0,
                    (0, 45, 0, 3, 2),
                    None,
                    false,
                    false,
                ),
            ],
        );
        let rows = build_upstream_rows(&now, Some(&prev), 1.0);
        // 期待順: beta (500 rps) → alpha (50 rps, tie で zone alphabetical) → gamma
        assert_eq!(rows[0].zone, "g/beta:1");
        assert_eq!(rows[1].zone, "g/alpha:1");
        assert_eq!(rows[2].zone, "g/gamma:1");
    }

    #[test]
    fn build_upstream_rows_carries_state_flags() {
        let s = status_with_upstreams(
            1000,
            &[
                ("g", "up:1", 0, 0, 0, 0, (0, 0, 0, 0, 0), None, false, false),
                (
                    "g",
                    "backup:1",
                    0,
                    0,
                    0,
                    0,
                    (0, 0, 0, 0, 0),
                    None,
                    true,
                    false,
                ),
                (
                    "g",
                    "down:1",
                    0,
                    0,
                    0,
                    0,
                    (0, 0, 0, 0, 0),
                    None,
                    false,
                    true,
                ),
            ],
        );
        let rows = build_upstream_rows(&s, None, 0.0);
        let find = |z: &str| rows.iter().find(|r| r.zone == z).unwrap().state;
        assert_eq!(find("g/up:1"), UpstreamState::Up);
        assert_eq!(find("g/backup:1"), UpstreamState::Backup);
        assert_eq!(find("g/down:1"), UpstreamState::Down);
    }

    // ---------- render (Upstream) ----------

    #[test]
    fn render_upstream_shows_headers_and_state_column() {
        let mut app = App::new();
        app.on_fetch_ok(status_with_upstreams(
            2000,
            &[
                (
                    "backend_api",
                    "127.0.0.1:9001",
                    100,
                    0,
                    0,
                    50,
                    (0, 100, 0, 0, 0),
                    None,
                    false,
                    false,
                ),
                (
                    "backend_api",
                    "127.0.0.1:9002",
                    50,
                    0,
                    0,
                    0,
                    (0, 50, 0, 0, 0),
                    None,
                    false,
                    true,
                ),
            ],
        ));
        let out = draw_upstream(&mut app, 100, 6);
        for h in &UPSTREAM_HEADERS {
            assert!(out.contains(h), "header {h} missing in:\n{out}");
        }
        // ZONE 列は group/host:port
        assert!(out.contains("backend_api/127.0.0.1:9001"), "out:\n{out}");
        // STATE 列: up / down (color theme なので英小文字)
        assert!(out.contains("up"), "out:\n{out}");
        assert!(out.contains("down"), "out:\n{out}");
    }

    #[test]
    fn render_upstream_mono_state_uses_bracket_letters() {
        let mut app = App::with_theme(crate::theme::Theme::mono());
        app.on_fetch_ok(status_with_upstreams(
            2000,
            &[("g", "h:1", 0, 0, 0, 0, (0, 0, 0, 0, 0), None, false, true)],
        ));
        let out = draw_upstream(&mut app, 100, 4);
        assert!(out.contains("[D]"), "mono down should be [D]:\n{out}");
    }

    #[test]
    fn render_upstream_with_no_snapshot_shows_placeholder() {
        let mut app = App::new();
        app.active_tab = Tab::Upstream;
        let out = draw(&app, 80, 3);
        assert!(
            out.contains("waiting for first VTS snapshot"),
            "out:\n{out}"
        );
    }

    #[test]
    fn upstream_fits_in_80x24() {
        let mut app = App::new();
        app.on_fetch_ok(status_with_upstreams(
            2000,
            &[(
                "backend_api",
                "127.0.0.1:9001",
                0,
                0,
                0,
                0,
                (0, 0, 0, 0, 0),
                None,
                false,
                false,
            )],
        ));
        let _ = draw_upstream(&mut app, 80, 24);
    }
}
