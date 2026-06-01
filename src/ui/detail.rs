//! Enter で開く詳細オーバーレイ (issue #32)。
//!
//! `app.detail_zone` が `Some(name)` のとき、画面中央 50% にモーダルを描画する。
//! 中身は 3 段:
//!
//! 1. **上段**: `p50 / p95 / p99` の数値。histogram 未設定 zone は
//!    `Average request_msec only` の 1 行に置き換える。
//! 2. **中段**: `BarChart` で 1 tick 分の bucket 別件数 (PDF) を可視化。
//!    vts の `requestBuckets.counters` は累積 (CDF) で返るので、隣接 bucket 間の
//!    差分を取って「その bucket レンジに入った件数」に変換してから描画する
//!    (issue #134)。軸ラベルは `requestBuckets.msecs` から実行時に組み立てる
//!    (ハードコード禁止)。histogram なし zone は `No histogram data` に置き換える。
//! 3. **下段**: 各レスポンス分類のカウント (`1xx`〜`5xx` の累積値)。
//!    cache zone は `hit / miss / bypass / expired / stale / updating / revalidated / scarce`。
//!
//! 表示対象は現在の `active_tab` に応じて切り替える:
//! - `Tab::Server`: `now.server_zones[name]` を引く
//! - `Tab::Upstream`: `detail_zone` は `"group/host:port"` 表記 (table と同じ規約)
//! - `Tab::Cache`: `now.cache_zones[name]` を引く。histogram は常に「なし」扱い。
//!
//! Esc での閉鎖は `main.rs::handle_key` 側で `app.detail_zone = None` を立てる。

use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{BarChart, Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::model::{Buckets, Responses, ServerZone, UpstreamServer, VtsStatus};
use crate::state::{percentile, App, PercentileResult, Tab};

/// 詳細オーバーレイで描画する 3 段ぶんの素材。
///
/// 描画関数 (`render_overlay`) と組み立て関数 (`build_detail`) を分けることで、
/// パーセンタイル算出 / bucket delta 計算をテストから直接検証できる。
#[derive(Debug, Clone, PartialEq)]
pub struct DetailView {
    /// オーバーレイのタイトルバーに出す zone 名。
    pub zone: String,
    /// p50 / p95 / p99 のセット。histogram なし zone では全 `Average` か `NoData`。
    pub percentiles: PercentileTriple,
    /// 中段の BarChart 用データ。`None` なら「No histogram data」メッセージを出す。
    pub histogram: Option<HistogramBars>,
    /// 下段の「responses ラベル: 値」一覧。Server/Upstream は 1xx-5xx、
    /// Cache は hit/miss 系。表示順は固定。
    pub responses: Vec<(&'static str, u64)>,
}

/// `(p50, p95, p99)` のラッパー。3 つを `Vec` に展開する手間を省くだけの型。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PercentileTriple {
    pub p50: PercentileResult,
    pub p95: PercentileResult,
    pub p99: PercentileResult,
}

/// BarChart に渡す `(label, value)` の組と最大値。
///
/// ratatui の `BarChart::data(&[(&str, u64)])` 形式に合わせて文字列 label を
/// 持つ。label は `requestBuckets.msecs[i]` から `<=Nms` の形で組み立てる
/// (`msecs` を超えた最終 bucket は `>Nms`)。
#[derive(Debug, Clone, PartialEq)]
pub struct HistogramBars {
    pub bars: Vec<(String, u64)>,
    pub max: u64,
}

impl HistogramBars {
    /// BarChart へ流すための借用ベクタ。`(&str, u64)` を要求する API に合わせる。
    fn as_borrowed(&self) -> Vec<(&str, u64)> {
        self.bars.iter().map(|(l, v)| (l.as_str(), *v)).collect()
    }
}

// ---------- build ----------

/// `app` の現在状態から 1 回ぶんの DetailView を組み立てる。
///
/// `app.detail_zone` が `None` の場合は `None` を返す (= 呼び出し側で「描画しない」)。
/// 選択中の zone が最新 snapshot に存在しなければ「データが消えた」状態として
/// `responses` を空に、percentiles を `NoData` にして空ビューを返す。
pub fn build_detail(app: &App) -> Option<DetailView> {
    let zone_name = app.detail_zone.as_ref()?;
    let now = app.history.latest()?;
    let prev = app.history.previous();

    match app.active_tab {
        Tab::Server => Some(build_server_detail(
            zone_name,
            &now.status,
            prev.map(|p| &p.status),
        )),
        Tab::Upstream => Some(build_upstream_detail(
            zone_name,
            &now.status,
            prev.map(|p| &p.status),
        )),
        Tab::Cache => Some(build_cache_detail(zone_name, &now.status)),
        Tab::Filter => Some(build_filter_detail(
            zone_name,
            &now.status,
            prev.map(|p| &p.status),
        )),
    }
}

fn build_server_detail(name: &str, now: &VtsStatus, prev: Option<&VtsStatus>) -> DetailView {
    let now_zone = now.server_zones.get(name);
    let prev_zone = prev.and_then(|p| p.server_zones.get(name));

    let (percentiles, histogram) = match now_zone {
        Some(z) => percentiles_and_bars_request(
            z.request_buckets.as_ref(),
            prev_zone.and_then(|pz| pz.request_buckets.as_ref()),
            z.request_msec,
        ),
        None => (
            PercentileTriple {
                p50: PercentileResult::NoData,
                p95: PercentileResult::NoData,
                p99: PercentileResult::NoData,
            },
            None,
        ),
    };

    let responses = now_zone
        .map(|z| http_response_breakdown(&z.responses))
        .unwrap_or_default();

    DetailView {
        zone: name.to_string(),
        percentiles,
        histogram,
        responses,
    }
}

fn build_upstream_detail(name: &str, now: &VtsStatus, prev: Option<&VtsStatus>) -> DetailView {
    // detail_zone は table と同じく "group/host:port" 形式 (CLAUDE.md 設計判断)。
    // 先頭の '/' で分割する。host:port 側にも ':' は来るが '/' は無い前提。
    let (group, server) = match name.split_once('/') {
        Some(p) => p,
        None => {
            return DetailView {
                zone: name.to_string(),
                percentiles: PercentileTriple {
                    p50: PercentileResult::NoData,
                    p95: PercentileResult::NoData,
                    p99: PercentileResult::NoData,
                },
                histogram: None,
                responses: Vec::new(),
            };
        }
    };

    let now_server = find_upstream(now, group, server);
    let prev_server = prev.and_then(|p| find_upstream(p, group, server));

    let (percentiles, histogram) = match now_server {
        Some(s) => percentiles_and_bars_request(
            s.request_buckets.as_ref(),
            prev_server.and_then(|ps| ps.request_buckets.as_ref()),
            s.request_msec,
        ),
        None => (
            PercentileTriple {
                p50: PercentileResult::NoData,
                p95: PercentileResult::NoData,
                p99: PercentileResult::NoData,
            },
            None,
        ),
    };

    let responses = now_server
        .map(|s| http_response_breakdown(&s.responses))
        .unwrap_or_default();

    DetailView {
        zone: name.to_string(),
        percentiles,
        histogram,
        responses,
    }
}

fn build_cache_detail(name: &str, now: &VtsStatus) -> DetailView {
    let now_zone = now.cache_zones.get(name);
    // cache zone は request histogram を持たないので常に「なし」扱い。
    // percentile は `Average` を載せても意味が無いため、全 NoData で揃える。
    let percentiles = PercentileTriple {
        p50: PercentileResult::NoData,
        p95: PercentileResult::NoData,
        p99: PercentileResult::NoData,
    };
    let responses = now_zone
        .map(|z| cache_response_breakdown(&z.responses))
        .unwrap_or_default();

    DetailView {
        zone: name.to_string(),
        percentiles,
        histogram: None,
        responses,
    }
}

fn build_filter_detail(name: &str, now: &VtsStatus, prev: Option<&VtsStatus>) -> DetailView {
    // detail_zone は table と同じく "group/key" 形式 (issue #45)。先頭の '/' で
    // 分割する。filter key 側に '/' は来ない前提 ($geoip_country_code 等)。
    let empty = DetailView {
        zone: name.to_string(),
        percentiles: PercentileTriple {
            p50: PercentileResult::NoData,
            p95: PercentileResult::NoData,
            p99: PercentileResult::NoData,
        },
        histogram: None,
        responses: Vec::new(),
    };
    let Some((group, key)) = name.split_once('/') else {
        return empty;
    };

    let now_zone = find_filter(now, group, key);
    let prev_zone = prev.and_then(|p| find_filter(p, group, key));

    let (percentiles, histogram) = match now_zone {
        Some(z) => percentiles_and_bars_request(
            z.request_buckets.as_ref(),
            prev_zone.and_then(|pz| pz.request_buckets.as_ref()),
            z.request_msec,
        ),
        None => return empty,
    };

    let responses = now_zone
        .map(|z| http_response_breakdown(&z.responses))
        .unwrap_or_default();

    DetailView {
        zone: name.to_string(),
        percentiles,
        histogram,
        responses,
    }
}

fn find_filter<'a>(s: &'a VtsStatus, group: &str, key: &str) -> Option<&'a ServerZone> {
    s.filter_zones.get(group).and_then(|keys| keys.get(key))
}

fn find_upstream<'a>(s: &'a VtsStatus, group: &str, server: &str) -> Option<&'a UpstreamServer> {
    s.upstream_zones
        .get(group)
        .and_then(|servers| servers.iter().find(|x| x.server == server))
}

/// histogram あり/なしで分岐した percentile 群と bar 群を返す。
///
/// - histogram あり (`now.msecs` 非空): `prev` があれば差分から p50/p95/p99 と
///   bar delta を組み立てる。`prev` 不在 (初 tick) は p* は `NoData`、bars は
///   現在の `counters` 累積値そのまま (delta 不能だが「histogram の形」を見せる
///   ためにフォールバック値を載せる)。
/// - histogram なし: p* は `Average(request_msec)` を 3 つ並べる
///   (UI が `Average request_msec only` 表示に切り替えるトリガ)。bars は `None`。
fn percentiles_and_bars_request(
    now: Option<&Buckets>,
    prev: Option<&Buckets>,
    request_msec_avg: u64,
) -> (PercentileTriple, Option<HistogramBars>) {
    let has_buckets = now.is_some_and(|b| !b.msecs.is_empty());
    if !has_buckets {
        let avg = percentile::average_fallback(request_msec_avg);
        return (
            PercentileTriple {
                p50: avg,
                p95: avg,
                p99: avg,
            },
            None,
        );
    }
    let now_b = now.expect("checked has_buckets");
    let (p50, p95, p99) = match prev {
        Some(prev_b) => (
            percentile::percentile(prev_b, now_b, 0.5),
            percentile::percentile(prev_b, now_b, 0.95),
            percentile::percentile(prev_b, now_b, 0.99),
        ),
        None => (
            PercentileResult::NoData,
            PercentileResult::NoData,
            PercentileResult::NoData,
        ),
    };
    let bars = bars_from_buckets(now_b, prev);
    (PercentileTriple { p50, p95, p99 }, Some(bars))
}

/// 1 tick ぶんの bucket 別件数 (PDF) を組み立てる (issue #134)。
///
/// vts の `requestBuckets.counters` は **累積 (CDF)** で「その閾値以下に入った
/// リクエスト数」を返す。`prev` との時間方向差分を取っただけでは形は CDF の
/// まま (e.g. 全件が最小バケットに収まると `<=5, <=10, <=50, ...` が全部同値で
/// 並ぶ) で histogram として読めないため、隣接 bucket 間の差分も取り
/// 「その bucket レンジに新たに入った件数」に変換する。これは Prometheus +
/// Grafana の histogram 表示 (`rate(le[i+1]) - rate(le[i])`) と同じ慣習。
///
/// 失敗系のフォールバック:
/// - `prev` 不在 / shape mismatch のときは `now.counters` を CDF とみなして
///   そのまま PDF 化する (初 tick でも「形」だけは見えるよう)。
fn bars_from_buckets(now: &Buckets, prev: Option<&Buckets>) -> HistogramBars {
    let len = now.msecs.len();
    let prev_counters: Option<&[u64]> = prev
        .filter(|p| p.msecs == now.msecs && p.counters.len() == now.counters.len())
        .map(|p| p.counters.as_slice());

    let mut bars: Vec<(String, u64)> = Vec::with_capacity(len);
    let mut prev_cum = 0u64;
    for i in 0..len {
        let now_c = *now.counters.get(i).unwrap_or(&0);
        let cum_delta = match prev_counters {
            Some(pc) => now_c.saturating_sub(*pc.get(i).unwrap_or(&0)),
            None => now_c,
        };
        let bin = cum_delta.saturating_sub(prev_cum);
        prev_cum = cum_delta;
        let label = bucket_label(&now.msecs, i);
        bars.push((label, bin));
    }
    let max = bars.iter().map(|(_, v)| *v).max().unwrap_or(0);
    HistogramBars { bars, max }
}

/// `requestBuckets.msecs` から bucket の表示 label を組み立てる。
///
/// 受け入れ条件「ハードコード禁止」。最終 bucket だけは `>` プレフィックスを
/// 付けて「上限を超えた件数」扱いを明示する。`bars_from_buckets` は隣接 bucket
/// 差分で PDF 化しているので、`<=N` は「直前 bucket 超〜N ms 以下に入った件数」、
/// `>N` は「N ms を超えた件数」を意味する。
fn bucket_label(msecs: &[u64], idx: usize) -> String {
    let last = msecs.len().saturating_sub(1);
    let ms = msecs.get(idx).copied().unwrap_or(0);
    if idx == last {
        format!(">{ms}")
    } else {
        format!("<={ms}")
    }
}

/// Server / Upstream zone の `Responses` を 1xx〜5xx に展開する。
///
/// 表示は累積値 (絶対値)。差分にしないのは「現在トータルで何件返したか」を
/// 見たい運用想定 (Phase 2 で window 差分版を出してもよい)。
fn http_response_breakdown(r: &Responses) -> Vec<(&'static str, u64)> {
    vec![
        ("1xx", r.r1xx),
        ("2xx", r.r2xx),
        ("3xx", r.r3xx),
        ("4xx", r.r4xx),
        ("5xx", r.r5xx),
    ]
}

/// Cache zone の `Responses` を hit/miss 系 8 種に展開する。
fn cache_response_breakdown(r: &Responses) -> Vec<(&'static str, u64)> {
    vec![
        ("hit", r.hit),
        ("miss", r.miss),
        ("bypass", r.bypass),
        ("expired", r.expired),
        ("stale", r.stale),
        ("updating", r.updating),
        ("revalidated", r.revalidated),
        ("scarce", r.scarce),
    ]
}

// ---------- format ----------

/// `PercentileResult` を `p50/p95/p99` 行表示用の文字列に整形する。
/// `Average` は 1 段で「Average only」表示にまとめるので、ここでは扱わない
/// (描画関数側で分岐)。
fn fmt_percentile(p: PercentileResult) -> String {
    match p {
        PercentileResult::Value(ms) => format!("{}ms", ms.round() as u64),
        PercentileResult::Overflow(max) => format!(">{max}ms"),
        PercentileResult::Average(ms) => format!("~{}ms", ms.round() as u64),
        PercentileResult::NoData => "—".to_string(),
    }
}

// ---------- render ----------

/// detail overlay を画面中央 (50%) に描画する。
///
/// 呼び出し側 (`ui::render`) で `app.detail_zone.is_some()` のときだけ呼ぶこと。
/// 描画前に `Clear` で背景を消す。
pub fn render_overlay(f: &mut Frame<'_>, app: &App, frame_area: Rect) {
    let Some(view) = build_detail(app) else {
        return;
    };

    let area = centered_rect(frame_area);
    f.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" zone: {} ", view.zone));
    let inner = block.inner(area);
    f.render_widget(block, area);

    // 上段: p* 行 = 1 行、下段: responses 行 = 1 行、中段: 残り全部 (BarChart or msg)。
    let [top, mid, bottom] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .areas(inner);

    f.render_widget(top_paragraph(&view, app), top);
    render_middle(f, &view, mid);
    f.render_widget(bottom_paragraph(&view, app), bottom);
}

/// 上段 (p50/p95/p99 もしくは Average only)。
fn top_paragraph<'a>(view: &'a DetailView, app: &App) -> Paragraph<'a> {
    // histogram なし zone は全 percentile が `Average(同値)` で揃うので 1 行表示。
    let line = if view.histogram.is_none() {
        match view.percentiles.p50 {
            PercentileResult::Average(ms) => Line::from(vec![
                Span::styled(
                    "Average request_msec only: ",
                    Style::default().add_modifier(Modifier::DIM),
                ),
                Span::styled(format!("~{}ms", ms.round() as u64), app.theme.header_value),
            ]),
            // NoData (zone 消失等) も同じ 1 行に倒す
            _ => Line::from(Span::styled(
                "Average request_msec only: —",
                Style::default().add_modifier(Modifier::DIM),
            )),
        }
    } else {
        Line::from(vec![
            Span::styled("p50 ", Style::default().add_modifier(Modifier::DIM)),
            Span::styled(fmt_percentile(view.percentiles.p50), app.theme.header_value),
            Span::raw("  "),
            Span::styled("p95 ", Style::default().add_modifier(Modifier::DIM)),
            Span::styled(fmt_percentile(view.percentiles.p95), app.theme.header_value),
            Span::raw("  "),
            Span::styled("p99 ", Style::default().add_modifier(Modifier::DIM)),
            Span::styled(fmt_percentile(view.percentiles.p99), app.theme.header_value),
        ])
    };
    Paragraph::new(line).alignment(Alignment::Left)
}

/// 中段 (BarChart もしくは「No histogram data」)。
fn render_middle(f: &mut Frame<'_>, view: &DetailView, area: Rect) {
    match &view.histogram {
        Some(h) => {
            let data = h.as_borrowed();
            // bar_width はターミナル幅に応じて 1〜4 で自動調整 (label と数値が
            // 重ならない最低限を確保)。label が "<=12345" まで来る想定で 6 桁。
            let bar_width = compute_bar_width(area.width, data.len());
            let chart = BarChart::default()
                .data(&data)
                .bar_width(bar_width)
                .bar_gap(1)
                .max(h.max.max(1));
            f.render_widget(chart, area);
        }
        None => {
            let p = Paragraph::new(Line::from(Span::styled(
                "No histogram data",
                Style::default().add_modifier(Modifier::DIM),
            )))
            .alignment(Alignment::Center);
            f.render_widget(p, area);
        }
    }
}

/// 下段 (responses ラベル / 値)。
fn bottom_paragraph<'a>(view: &'a DetailView, app: &App) -> Paragraph<'a> {
    if view.responses.is_empty() {
        return Paragraph::new(Line::from(Span::styled(
            "(no response counters)",
            Style::default().add_modifier(Modifier::DIM),
        )));
    }
    let mut spans: Vec<Span<'a>> = Vec::with_capacity(view.responses.len() * 3);
    for (i, (label, value)) in view.responses.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled(
            format!("{label} "),
            Style::default().add_modifier(Modifier::DIM),
        ));
        spans.push(Span::styled(value.to_string(), app.theme.header_value));
    }
    Paragraph::new(Line::from(spans))
}

/// 中段に描く BarChart 用の bar 幅を、利用可能幅と bar 本数から決める。
///
/// ratatui の `BarChart` は各 bar の上に label を出すため `bar_width >= 1` で
/// 描画は可能だが、できれば label 文字列が読める幅 (最小 4) を取りたい。
/// 上限は 6。本数が幅を完全に超える場合は最低 1。
fn compute_bar_width(area_width: u16, bar_count: usize) -> u16 {
    if bar_count == 0 {
        return 1;
    }
    // gap=1 を含めた 1 bar あたりの占有幅 = bar_width + 1
    // area_width >= bar_count * (bar_width + 1) になるよう bar_width を決める。
    let n = bar_count as u16;
    let per_bar = area_width.saturating_div(n);
    // 端数を gap に取られるぶんを引いて 1 以上 6 以下にクランプ。
    per_bar.saturating_sub(1).clamp(1, 6)
}

/// 画面中央 50% (幅・高さ共に 50%) の矩形を返す。
///
/// 受け入れ条件「中央 50%」。極端な resize でも飛び出さないよう
/// `min(frame_size, ...)` でクランプする。
fn centered_rect(frame: Rect) -> Rect {
    let width = (frame.width / 2).max(20).min(frame.width);
    let height = (frame.height / 2).max(8).min(frame.height);

    let [_, mid_v, _] = Layout::vertical([
        Constraint::Length(frame.height.saturating_sub(height) / 2),
        Constraint::Length(height),
        Constraint::Min(0),
    ])
    .areas(frame);

    let [_, center, _] = Layout::horizontal([
        Constraint::Length(frame.width.saturating_sub(width) / 2),
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

    use crate::model::VtsStatus;

    type ZoneSpec<'a> = (
        &'a str,
        u64, // request_counter
        u64, // request_msec
        (u64, u64, u64, u64, u64),
        Option<(Vec<u64>, Vec<u64>)>,
    );

    fn status_with_server_zones(now_msec: u64, zones: &[ZoneSpec<'_>]) -> VtsStatus {
        let server_zones: serde_json::Map<String, serde_json::Value> = zones
            .iter()
            .map(|(name, rc, rms, (r1, r2, r3, r4, r5), buckets)| {
                let buckets_json = match buckets {
                    Some((msecs, counters)) => serde_json::json!({
                        "msecs": msecs,
                        "counters": counters,
                    }),
                    None => serde_json::json!({ "msecs": [], "counters": [] }),
                };
                let v = serde_json::json!({
                    "requestCounter": rc,
                    "inBytes": 0,
                    "outBytes": 0,
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
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| render_overlay(f, app, f.area()))
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

    // ---------- build_detail ----------

    #[test]
    fn build_detail_returns_none_when_no_zone_selected() {
        let app = App::new();
        assert!(build_detail(&app).is_none());
    }

    #[test]
    fn build_detail_returns_none_when_history_empty() {
        let mut app = App::new();
        app.detail_zone = Some("missing".into());
        assert!(build_detail(&app).is_none());
    }

    #[test]
    fn server_with_histogram_computes_percentiles_and_bars() {
        // prev = all 0, now = [10, 50, 100, 150, 200] over msecs [5,10,50,100,500].
        // delta = same as now. total = 200. p50 target=100 → between msecs[1]=10 and
        // msecs[2]=50. percentile module の振る舞いは percentile.rs::tests で確定済み。
        let prev = status_with_server_zones(
            1000,
            &[(
                "z",
                0,
                0,
                (0, 0, 0, 0, 0),
                Some((vec![5, 10, 50, 100, 500], vec![0, 0, 0, 0, 0])),
            )],
        );
        let now = status_with_server_zones(
            2000,
            &[(
                "z",
                200,
                0,
                (0, 100, 0, 0, 0),
                Some((vec![5, 10, 50, 100, 500], vec![10, 50, 100, 150, 200])),
            )],
        );
        let mut app = App::new();
        app.on_fetch_ok(prev);
        app.on_fetch_ok(now);
        app.detail_zone = Some("z".to_string());

        let v = build_detail(&app).expect("detail");
        // p50 < p95 < p99 で値が立つこと
        assert!(matches!(v.percentiles.p50, PercentileResult::Value(_)));
        assert!(matches!(v.percentiles.p95, PercentileResult::Value(_)));
        assert!(matches!(v.percentiles.p99, PercentileResult::Value(_)));
        // histogram bars が 5 本生成され、ラベルは msecs から作られていること
        let h = v.histogram.expect("histogram present");
        assert_eq!(h.bars.len(), 5);
        assert_eq!(h.bars[0].0, "<=5");
        assert_eq!(h.bars[1].0, "<=10");
        // 最終 bucket は ">" プレフィックス
        assert_eq!(h.bars[4].0, ">500");
        // PDF 化 (issue #134): vts は CDF を返すので隣接 bucket 差分を取った
        // 値 ([10, 50-10, 100-50, 150-100, 200-150]) が bar value になる。
        let values: Vec<u64> = h.bars.iter().map(|(_, v)| *v).collect();
        assert_eq!(values, vec![10, 40, 50, 50, 50]);
        assert_eq!(h.max, 50);
        // responses は累積で 2xx=100 が見える
        assert!(v.responses.contains(&("2xx", 100)));
    }

    #[test]
    fn server_without_histogram_falls_back_to_average() {
        let s = status_with_server_zones(1000, &[("z", 0, 42, (0, 5, 0, 0, 0), None)]);
        let mut app = App::new();
        app.on_fetch_ok(s);
        app.detail_zone = Some("z".to_string());

        let v = build_detail(&app).expect("detail");
        assert!(
            matches!(v.percentiles.p50, PercentileResult::Average(x) if (x - 42.0).abs() < 1e-9)
        );
        assert!(matches!(v.percentiles.p95, PercentileResult::Average(_)));
        assert!(matches!(v.percentiles.p99, PercentileResult::Average(_)));
        assert!(v.histogram.is_none(), "histogram should be None");
        // responses は累積
        assert!(v.responses.contains(&("2xx", 5)));
    }

    #[test]
    fn server_with_histogram_but_no_prev_has_nodata_percentiles_but_renders_bars() {
        // 初 tick: histogram の差分は不能 → p* は NoData。bars は累積値を
        // CDF とみなして PDF 化 ([5, 7-5] = [5, 2])。
        let s = status_with_server_zones(
            1000,
            &[("z", 0, 0, (0, 0, 0, 0, 0), Some((vec![10, 50], vec![5, 7])))],
        );
        let mut app = App::new();
        app.on_fetch_ok(s);
        app.detail_zone = Some("z".into());

        let v = build_detail(&app).expect("detail");
        assert!(matches!(v.percentiles.p50, PercentileResult::NoData));
        let h = v.histogram.expect("present");
        assert_eq!(
            h.bars.iter().map(|(_, v)| *v).collect::<Vec<_>>(),
            vec![5, 2]
        );
    }

    #[test]
    fn histogram_pdf_only_first_bin_when_all_in_lowest_bucket() {
        // issue #134 回帰: vts の CDF をそのまま並べると `<=5, <=10, <=50, ...`
        // が全部同じ値で並ぶ (= histogram として読めない)。PDF 化されているなら
        // 「全件 5ms 以下」の状況で最初の bar だけ立ち、残りは 0 になるはず。
        let prev = status_with_server_zones(
            1000,
            &[(
                "z",
                0,
                0,
                (0, 0, 0, 0, 0),
                Some((vec![5, 10, 50, 100, 500], vec![0, 0, 0, 0, 0])),
            )],
        );
        let now = status_with_server_zones(
            2000,
            &[(
                "z",
                100,
                0,
                (0, 100, 0, 0, 0),
                // CDF: 全 100 件が <=5ms に入ったので各境界も累積 100
                Some((vec![5, 10, 50, 100, 500], vec![100, 100, 100, 100, 100])),
            )],
        );
        let mut app = App::new();
        app.on_fetch_ok(prev);
        app.on_fetch_ok(now);
        app.detail_zone = Some("z".into());

        let h = build_detail(&app)
            .expect("detail")
            .histogram
            .expect("histogram present");
        let values: Vec<u64> = h.bars.iter().map(|(_, v)| *v).collect();
        assert_eq!(values, vec![100, 0, 0, 0, 0]);
        assert_eq!(h.max, 100);
    }

    #[test]
    fn missing_zone_returns_empty_view() {
        let s = status_with_server_zones(1000, &[("other", 0, 0, (0, 0, 0, 0, 0), None)]);
        let mut app = App::new();
        app.on_fetch_ok(s);
        app.detail_zone = Some("missing".into());

        let v = build_detail(&app).expect("detail (zone absent → empty view)");
        assert!(matches!(v.percentiles.p50, PercentileResult::NoData));
        assert!(v.histogram.is_none());
        assert!(v.responses.is_empty());
    }

    #[test]
    fn bucket_labels_use_runtime_msecs_not_hardcoded() {
        // 受け入れ条件「軸ラベルは requestBuckets.msecs から実行時生成（ハードコード禁止）」
        let prev = status_with_server_zones(
            1000,
            &[(
                "z",
                0,
                0,
                (0, 0, 0, 0, 0),
                Some((vec![7, 99, 9999], vec![0, 0, 0])),
            )],
        );
        let now = status_with_server_zones(
            2000,
            &[(
                "z",
                10,
                0,
                (0, 10, 0, 0, 0),
                Some((vec![7, 99, 9999], vec![3, 7, 10])),
            )],
        );
        let mut app = App::new();
        app.on_fetch_ok(prev);
        app.on_fetch_ok(now);
        app.detail_zone = Some("z".into());

        let v = build_detail(&app).expect("detail");
        let h = v.histogram.expect("present");
        let labels: Vec<String> = h.bars.iter().map(|(l, _)| l.clone()).collect();
        assert_eq!(labels, vec!["<=7", "<=99", ">9999"]);
    }

    // ---------- render ----------

    #[test]
    fn render_with_histogram_shows_p_labels_and_zone_title() {
        // 受け入れ条件: histogram あり zone で Enter → BarChart が出る
        let prev = status_with_server_zones(
            1000,
            &[(
                "alpha",
                0,
                0,
                (0, 0, 0, 0, 0),
                Some((vec![10, 50, 100], vec![0, 0, 0])),
            )],
        );
        let now = status_with_server_zones(
            2000,
            &[(
                "alpha",
                100,
                0,
                (0, 100, 0, 0, 0),
                Some((vec![10, 50, 100], vec![20, 60, 100])),
            )],
        );
        let mut app = App::new();
        app.on_fetch_ok(prev);
        app.on_fetch_ok(now);
        app.detail_zone = Some("alpha".into());

        let out = draw(&app, 80, 24);
        assert!(out.contains("zone: alpha"), "title missing:\n{out}");
        assert!(out.contains("p50"), "p50 missing:\n{out}");
        assert!(out.contains("p95"), "p95 missing:\n{out}");
        assert!(out.contains("p99"), "p99 missing:\n{out}");
        // 中段に「No histogram data」が出ていないこと (= BarChart が出ている)
        assert!(
            !out.contains("No histogram data"),
            "should not show 'No histogram data' when buckets present:\n{out}"
        );
        // 下段の responses ラベル
        assert!(out.contains("2xx"), "responses 2xx missing:\n{out}");
    }

    #[test]
    fn render_without_histogram_shows_no_data_message() {
        // 受け入れ条件: histogram なし zone で Enter → 中段は "No histogram data"
        let s = status_with_server_zones(1000, &[("beta", 0, 33, (0, 5, 0, 0, 0), None)]);
        let mut app = App::new();
        app.on_fetch_ok(s);
        app.detail_zone = Some("beta".into());

        let out = draw(&app, 80, 24);
        assert!(out.contains("zone: beta"), "title missing:\n{out}");
        assert!(
            out.contains("No histogram data"),
            "expected 'No histogram data' line:\n{out}"
        );
        // Average only ラベルが出ること
        assert!(
            out.contains("Average request_msec only"),
            "Average label missing:\n{out}"
        );
        // 値も載る
        assert!(out.contains("~33ms"), "~33ms missing:\n{out}");
    }

    #[test]
    fn render_no_op_when_detail_zone_is_none() {
        // detail_zone = None → 何も描画しない (背景空白のまま)
        let app = App::new();
        let out = draw(&app, 80, 24);
        assert!(!out.contains("zone:"));
        assert!(!out.contains("p50"));
    }

    #[test]
    fn render_fits_in_80x24_layout() {
        let prev = status_with_server_zones(
            1000,
            &[(
                "z",
                0,
                0,
                (0, 0, 0, 0, 0),
                Some((vec![10, 50, 100], vec![0, 0, 0])),
            )],
        );
        let now = status_with_server_zones(
            2000,
            &[(
                "z",
                10,
                0,
                (0, 10, 0, 0, 0),
                Some((vec![10, 50, 100], vec![5, 8, 10])),
            )],
        );
        let mut app = App::new();
        app.on_fetch_ok(prev);
        app.on_fetch_ok(now);
        app.detail_zone = Some("z".into());
        let _ = draw(&app, 80, 24);
    }

    // ---------- compute_bar_width ----------

    #[test]
    fn compute_bar_width_clamps_between_1_and_6() {
        // 余裕がある場合は 6 がキャップ
        assert_eq!(compute_bar_width(100, 3), 6);
        // 狭い場合でも 1 を下回らない
        assert_eq!(compute_bar_width(3, 5), 1);
        // 0 本のとき panic しない
        assert_eq!(compute_bar_width(10, 0), 1);
    }

    // ---------- filter detail (issue #45) ----------

    /// `filterZones["country::*"]["US"]` 1 件だけ持つ VtsStatus を作る。
    fn status_with_filter_zone(now_msec: u64, rms: u64, r2: u64) -> VtsStatus {
        let raw = serde_json::json!({
            "hostName": "h", "nginxVersion": "1", "moduleVersion": "v",
            "loadMsec": 0u64, "nowMsec": now_msec,
            "connections": {
                "active": 0, "reading": 0, "writing": 0,
                "waiting": 0, "accepted": 0, "handled": 0, "requests": 0
            },
            "filterZones": {
                "country::*": {
                    "US": {
                        "requestCounter": 0, "inBytes": 0, "outBytes": 0,
                        "responses": {
                            "1xx": 0, "2xx": r2, "3xx": 0, "4xx": 0, "5xx": 0,
                            "miss": 0, "bypass": 0, "expired": 0, "stale": 0,
                            "updating": 0, "revalidated": 0, "hit": 0, "scarce": 0,
                        },
                        "requestMsec": rms, "requestMsecCounter": 0,
                        "requestBuckets": { "msecs": [], "counters": [] },
                    }
                }
            },
        });
        serde_json::from_value(raw).unwrap()
    }

    #[test]
    fn filter_detail_resolves_group_slash_key() {
        let s = status_with_filter_zone(1000, 33, 5);
        let mut app = App::new();
        app.active_tab = Tab::Filter;
        app.on_fetch_ok(s);
        app.detail_zone = Some("country::*/US".into());

        let v = build_detail(&app).expect("detail");
        assert_eq!(v.zone, "country::*/US");
        // histogram なし key は Average fallback
        assert!(
            matches!(v.percentiles.p50, PercentileResult::Average(x) if (x - 33.0).abs() < 1e-9)
        );
        assert!(v.histogram.is_none());
        assert!(v.responses.contains(&("2xx", 5)));
    }

    #[test]
    fn filter_detail_missing_key_returns_empty_view() {
        let s = status_with_filter_zone(1000, 0, 0);
        let mut app = App::new();
        app.active_tab = Tab::Filter;
        app.on_fetch_ok(s);
        app.detail_zone = Some("country::*/ZZ".into());

        let v = build_detail(&app).expect("detail (key absent → empty)");
        assert!(matches!(v.percentiles.p50, PercentileResult::NoData));
        assert!(v.histogram.is_none());
        assert!(v.responses.is_empty());
    }
}
