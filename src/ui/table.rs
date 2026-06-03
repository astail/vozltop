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
//! ## ソート (2 層構成)
//!
//! `build_*_rows` は決定的なベース順 (Server/Upstream: RPS 降順、Cache: HIT%
//! 降順、tie は ZONE 名昇順) を返す。HashMap 由来の非決定的順序を吸収し、build
//! 出力を呼び出し側・テストから決定的に扱えるようにするための層
//! (`sort_*_rows_default`)。
//!
//! Server / Upstream タブは render / `selected_zone` が `App::sort` を反映した
//! 動的ソート (`sort_server_rows` / `sort_upstream_rows`) をこの上に重ねるため、
//! 最終表示順はユーザー操作 (1-9 / F5) で決まる (issue #31)。Cache タブの動的
//! ソートは未実装なので、`render_cache` は build のベース順をそのまま表示する。
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
use ratatui::widgets::{
    Block, BorderType, Borders, Cell, Padding, Paragraph, Row, Table, TableState,
};
use ratatui::Frame;

use crate::model::{Responses, ServerZone, UpstreamServer, VtsStatus};
use crate::state::{percentile, AlertConfig, App, PercentileResult, SortColumn, SortState, Tab};
use crate::theme::Theme;
use crate::ui::header::format_bps;

/// 8 列ヘッダ。`ui::table::tests::*` から覗くことを想定して pub const にしてある。
pub const SERVER_HEADERS: [&str; 8] = [
    "ZONE", "RPS", "2xx%", "4xx%", "5xx%", "p95", "IN/s", "OUT/s",
];

/// 9 列ヘッダ (Upstream タブ)。Server タブ + STATE。
pub const UPSTREAM_HEADERS: [&str; 9] = [
    "ZONE", "RPS", "2xx%", "4xx%", "5xx%", "p95", "IN/s", "OUT/s", "STATE",
];

/// 8 列ヘッダ (Cache タブ)。cacheZones は request_counter / latency を持たない
/// ため Server / Upstream とは列構成が異なる (HIT% / MISS / EXPIRED / STALE / USED)。
pub const CACHE_HEADERS: [&str; 8] = [
    "ZONE", "HIT%", "MISS", "EXPIRED", "STALE", "USED", "IN/s", "OUT/s",
];

/// 8 列ヘッダ (Filter タブ)。filterZones の各 key は serverZones と同形の
/// stats を持つため、列構成は Server タブと同一 (ZONE 列のみ `group/key` 表記)。
pub const FILTER_HEADERS: [&str; 8] = [
    "ZONE", "RPS", "2xx%", "4xx%", "5xx%", "p95", "IN/s", "OUT/s",
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
    /// issue #29: `up` 先頭 → `backup` → `down` (ヘルス把握優先)。issue #31 で
    /// `App::sort` 経由の STATE 列ソート (`sort_upstream_rows`) から利用する。
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

/// build のベースソート (RPS 降順、tie は ZONE 名昇順)。
///
/// Server タブは render / `selected_zone` が `sort_server_rows` で `App::sort` を
/// 反映するため、この順は最終表示には出ない。build 出力を決定的に保つための層。
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

/// build のベースソート (RPS 降順、tie は ZONE 名昇順)。
///
/// Server タブ (`sort_server_rows_default`) と同じ規約・役割。STATE 列を含む
/// 動的ソートは render / `selected_zone` が `sort_upstream_rows` で `App::sort`
/// を反映してこの上に重ねる (issue #31)。
pub(crate) fn sort_upstream_rows_default(rows: &mut [UpstreamRow]) {
    rows.sort_by(|a, b| {
        b.rps
            .partial_cmp(&a.rps)
            .unwrap_or(Ordering::Equal)
            .then_with(|| a.zone.cmp(&b.zone))
    });
}

// ---------- 動的ソート / フィルタ (issue #31) ----------
//
// `App::sort` (`SortState`) と `App::filter` を反映する。render と
// `selected_zone` が同じ並び順を共有できるよう、本ファイルに集約する。
//
// ソート規約:
// - 数値列 (RPS / ratios / BW): `descending` で大小を反転。tie は ZONE 名昇順で
//   安定化 (HashMap 由来の非決定的順序を吸収)。
// - p95 列: histogram 群 → Average 群 → NoData 群の tier 分離が必須なので
//   `percentile::compare_for_sort` を使う。`descending` は **tier 内の数値だけ**
//   反転させ、tier 自体の並び (Value 群が上 / NoData が下) は固定する。
// - ZONE / STATE 列: 文字列 / state rank で比較。
//
// `Option<f64>` 列 (ratios) は `None` (= データ無し) を常に末尾へ送る
// (`descending` でも先頭に来ないよう tier 扱い)。

/// `f64` の昇順比較。`descending` で反転する。tie は呼び出し側で ZONE 名により
/// 安定化する。
fn cmp_f64(a: f64, b: f64, descending: bool) -> Ordering {
    let ord = a.partial_cmp(&b).unwrap_or(Ordering::Equal);
    if descending {
        ord.reverse()
    } else {
        ord
    }
}

/// `Option<f64>` 列の比較。`None` は方向に関わらず常に末尾。
fn cmp_opt_f64(a: Option<f64>, b: Option<f64>, descending: bool) -> Ordering {
    match (a, b) {
        (Some(x), Some(y)) => cmp_f64(x, y, descending),
        (Some(_), None) => Ordering::Less, // 値あり < データ無し
        (None, Some(_)) => Ordering::Greater, // データ無しは末尾
        (None, None) => Ordering::Equal,
    }
}

/// p95 列の比較。tier (Value 群 → Average 群 → NoData 群) は固定で、`descending`
/// は同 tier 内の数値順だけを反転する。
fn cmp_p95(a: &PercentileResult, b: &PercentileResult, descending: bool) -> Ordering {
    let asc = percentile::compare_for_sort(a, b);
    if !descending {
        return asc;
    }
    // 降順: tier はそのまま、同 tier 内のみ反転する。
    a.sort_tier().cmp(&b.sort_tier()).then_with(|| {
        // 同 tier なので compare_for_sort の戻り値 = 数値順。これを反転。
        percentile::compare_for_sort(a, b).reverse()
    })
}

/// Server 行を `SortState` に従って in-place ソートする。tie は ZONE 名昇順。
pub(crate) fn sort_server_rows(rows: &mut [ServerRow], sort: SortState) {
    let col = SortColumn::server_at(sort.column);
    let desc = sort.descending;
    rows.sort_by(|a, b| {
        let primary = match col {
            SortColumn::Zone => cmp_str(&a.zone, &b.zone, desc),
            SortColumn::Rps => cmp_f64(a.rps, b.rps, desc),
            SortColumn::R2xx => cmp_opt_f64(a.r2xx_pct, b.r2xx_pct, desc),
            SortColumn::R4xx => cmp_opt_f64(a.r4xx_pct, b.r4xx_pct, desc),
            SortColumn::R5xx => cmp_opt_f64(a.r5xx_pct, b.r5xx_pct, desc),
            SortColumn::P95 => cmp_p95(&a.p95, &b.p95, desc),
            SortColumn::InPerSec => cmp_f64(a.bw_in_per_sec, b.bw_in_per_sec, desc),
            SortColumn::OutPerSec => cmp_f64(a.bw_out_per_sec, b.bw_out_per_sec, desc),
            // Server タブに無い列 (STATE / Cache 系)。`server_at` が範囲外を ZONE に
            // 倒すため実際には到達しないが、網羅性のため ZONE 名で安定ソートする。
            _ => cmp_str(&a.zone, &b.zone, desc),
        };
        primary.then_with(|| a.zone.cmp(&b.zone))
    });
}

/// Upstream 行を `SortState` に従って in-place ソートする。tie は ZONE 名昇順。
pub(crate) fn sort_upstream_rows(rows: &mut [UpstreamRow], sort: SortState) {
    let col = SortColumn::upstream_at(sort.column);
    let desc = sort.descending;
    rows.sort_by(|a, b| {
        let primary = match col {
            SortColumn::Zone => cmp_str(&a.zone, &b.zone, desc),
            SortColumn::Rps => cmp_f64(a.rps, b.rps, desc),
            SortColumn::R2xx => cmp_opt_f64(a.r2xx_pct, b.r2xx_pct, desc),
            SortColumn::R4xx => cmp_opt_f64(a.r4xx_pct, b.r4xx_pct, desc),
            SortColumn::R5xx => cmp_opt_f64(a.r5xx_pct, b.r5xx_pct, desc),
            SortColumn::P95 => cmp_p95(&a.p95, &b.p95, desc),
            SortColumn::InPerSec => cmp_f64(a.bw_in_per_sec, b.bw_in_per_sec, desc),
            SortColumn::OutPerSec => cmp_f64(a.bw_out_per_sec, b.bw_out_per_sec, desc),
            SortColumn::State => {
                let ord = a.state.sort_rank().cmp(&b.state.sort_rank());
                if desc {
                    ord.reverse()
                } else {
                    ord
                }
            }
            // Upstream タブに無い列 (Cache 系)。`upstream_at` が範囲外を ZONE に
            // 倒すため実際には到達しないが、網羅性のため ZONE 名で安定ソートする。
            _ => cmp_str(&a.zone, &b.zone, desc),
        };
        primary.then_with(|| a.zone.cmp(&b.zone))
    });
}

/// 文字列の昇順比較。`descending` で反転する。
fn cmp_str(a: &str, b: &str, descending: bool) -> Ordering {
    let ord = a.cmp(b);
    if descending {
        ord.reverse()
    } else {
        ord
    }
}

/// zone 名 substring フィルタ (大文字小文字無視)。`filter` が空なら何もしない。
fn retain_matching<F>(rows: &mut Vec<F>, filter: &str, zone_of: impl Fn(&F) -> &str) {
    if filter.is_empty() {
        return;
    }
    let needle = filter.to_lowercase();
    rows.retain(|r| zone_of(r).to_lowercase().contains(&needle));
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

/// Cache タブ 1 行ぶんの確定済み描画データ。
///
/// cacheZones は `request_counter` / latency histogram を持たないため Server /
/// Upstream とは別の列構成。`hit_pct` は累積カウンタからの絶対値 (分母 0 で
/// `None`)。`miss` / `expired` / `stale` は累積カウント。`used_size` / `max_size`
/// は bytes で、`bw_*` のみ前 snapshot との差分から算出する。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CacheRow {
    pub zone: String,
    pub hit_pct: Option<f64>,
    pub miss: u64,
    pub expired: u64,
    pub stale: u64,
    pub used_size: u64,
    pub max_size: u64,
    pub bw_in_per_sec: f64,
    pub bw_out_per_sec: f64,
}

/// `now` (最新 snapshot) と `prev` (前 snapshot)、`dt_secs` (経過秒) から
/// Cache タブの行を組み立てる。
///
/// hit% / MISS / EXPIRED / STALE / USED は累積カウンタ・サイズの絶対値なので
/// `prev` 不要 (BW のみ差分)。`prev` が `None` / `dt_secs <= 0` の初 tick は
/// BW を 0 にする。
pub(crate) fn build_cache_rows(
    now: &VtsStatus,
    prev: Option<&VtsStatus>,
    dt_secs: f64,
) -> Vec<CacheRow> {
    let mut rows: Vec<CacheRow> = now
        .cache_zones
        .iter()
        .map(|(name, zone)| {
            let prev_zone = prev.and_then(|p| p.cache_zones.get(name));
            let bw_in = per_sec(prev_zone.map(|z| z.in_bytes), zone.in_bytes, dt_secs);
            let bw_out = per_sec(prev_zone.map(|z| z.out_bytes), zone.out_bytes, dt_secs);
            CacheRow {
                zone: name.clone(),
                hit_pct: cache_hit_pct(&zone.responses),
                miss: zone.responses.miss,
                expired: zone.responses.expired,
                stale: zone.responses.stale,
                used_size: zone.used_size,
                max_size: zone.max_size,
                bw_in_per_sec: bw_in,
                bw_out_per_sec: bw_out,
            }
        })
        .collect();

    sort_cache_rows_default(&mut rows);
    rows
}

/// Cache タブ build のベースソート (HIT% 降順、`None` (分母 0) は末尾、
/// tie は ZONE 名昇順)。
///
/// HashMap 由来の非決定的順序を吸収する決定論的初期順。`render_cache` /
/// `selected_zone` はこの上に [`sort_cache_rows`] で `App::sort` を反映する。
pub(crate) fn sort_cache_rows_default(rows: &mut [CacheRow]) {
    rows.sort_by(|a, b| {
        match (a.hit_pct, b.hit_pct) {
            // 両方値あり: HIT% 降順
            (Some(av), Some(bv)) => bv.partial_cmp(&av).unwrap_or(Ordering::Equal),
            // 片方のみ値あり: 値ありを先頭、None を末尾
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        }
        .then_with(|| a.zone.cmp(&b.zone))
    });
}

/// Cache 行を `SortState` に従って in-place ソートする。tie は ZONE 名昇順
/// (Server / Upstream / Filter と同じ規約)。
///
/// 列ごとの比較規約:
///
/// - `ZONE`: 文字列順
/// - `HitPct`: `Option<f64>` を `cmp_opt_f64` で比較 (`None` = 分母 0 は末尾)
/// - `Miss` / `Expired` / `Stale`: `u64` 数値順
/// - `Used`: `used_size / max_size` の比率で比較。`max_size == 0` は計算不能
///   なので末尾に送る (絶対 used_size ではなく比率で並べる方が「ほぼ満杯の
///   zone を上に」という運用が直感的)
/// - `InPerSec` / `OutPerSec`: `f64` 数値順
pub(crate) fn sort_cache_rows(rows: &mut [CacheRow], sort: SortState) {
    let col = SortColumn::cache_at(sort.column);
    let desc = sort.descending;
    rows.sort_by(|a, b| {
        let primary = match col {
            SortColumn::Zone => cmp_str(&a.zone, &b.zone, desc),
            SortColumn::HitPct => cmp_opt_f64(a.hit_pct, b.hit_pct, desc),
            SortColumn::Miss => cmp_u64(a.miss, b.miss, desc),
            SortColumn::Expired => cmp_u64(a.expired, b.expired, desc),
            SortColumn::Stale => cmp_u64(a.stale, b.stale, desc),
            SortColumn::Used => cmp_opt_f64(used_ratio(a), used_ratio(b), desc),
            SortColumn::InPerSec => cmp_f64(a.bw_in_per_sec, b.bw_in_per_sec, desc),
            SortColumn::OutPerSec => cmp_f64(a.bw_out_per_sec, b.bw_out_per_sec, desc),
            // Cache タブに無い列 (Server / Upstream / Filter 系)。`cache_at` が範囲外を
            // ZONE に倒すため実際には到達しないが、網羅性のため ZONE 名で安定ソートする。
            _ => cmp_str(&a.zone, &b.zone, desc),
        };
        primary.then_with(|| a.zone.cmp(&b.zone))
    });
}

/// USED 列ソート用の比率 (= `used_size / max_size`)。`max_size == 0` のときは
/// 比率が計算できないので `None` を返し、`cmp_opt_f64` が末尾に送る。
fn used_ratio(r: &CacheRow) -> Option<f64> {
    if r.max_size == 0 {
        None
    } else {
        Some(r.used_size as f64 / r.max_size as f64)
    }
}

/// `u64` の昇順比較。`descending` で反転する。tie は呼び出し側で ZONE 名により
/// 安定化する。
fn cmp_u64(a: u64, b: u64, descending: bool) -> Ordering {
    let ord = a.cmp(&b);
    if descending {
        ord.reverse()
    } else {
        ord
    }
}

/// Filter タブ 1 行ぶんの確定済み描画データ。
///
/// filterZones は `group -> key -> stats` の 2 段ネストを 1 key = 1 行に展開し、
/// ZONE 列に `"group/key"` を入れる (CLAUDE.md「Upstream 行の粒度」と同じ
/// nested data の規約)。stats は serverZones と同形なので [`ServerRow`] と同じ
/// 列を持つ。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct FilterRow {
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
/// Filter タブの行を組み立てる。
///
/// `filter_zones: HashMap<group, HashMap<key, ServerZone>>` を 1 key = 1 行に
/// 展開し、ZONE 列に `"group/key"` を入れる。差分計算は Server タブと完全に
/// 同じ (各 key の stats が `ServerZone` なので `compute_ratios` / `compute_p95`
/// をそのまま使う)。
pub(crate) fn build_filter_rows(
    now: &VtsStatus,
    prev: Option<&VtsStatus>,
    dt_secs: f64,
) -> Vec<FilterRow> {
    let dt_ok = dt_secs > 0.0;
    let mut rows: Vec<FilterRow> = Vec::new();
    for (group, keys) in &now.filter_zones {
        let prev_group = prev.and_then(|p| p.filter_zones.get(group));
        for (key, zone) in keys {
            let prev_zone = prev_group.and_then(|g| g.get(key));
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
            rows.push(FilterRow {
                zone: format!("{group}/{key}"),
                rps,
                r2xx_pct: r2,
                r4xx_pct: r4,
                r5xx_pct: r5,
                p95,
                bw_in_per_sec: bw_in,
                bw_out_per_sec: bw_out,
            });
        }
    }
    sort_filter_rows_default(&mut rows);
    rows
}

/// デフォルトソート (RPS 降順、tie は ZONE 名昇順)。Server タブと同じ規約。
///
/// Filter タブは render / `selected_zone` が `sort_filter_rows` で `App::sort` を
/// 反映するため、この順は最終表示には出ない。build 出力を決定的に保つための層。
pub(crate) fn sort_filter_rows_default(rows: &mut [FilterRow]) {
    rows.sort_by(|a, b| {
        b.rps
            .partial_cmp(&a.rps)
            .unwrap_or(Ordering::Equal)
            .then_with(|| a.zone.cmp(&b.zone))
    });
}

/// Filter 行を `SortState` に従って in-place ソートする。tie は ZONE 名昇順。
/// 列構成は Server と同一なので `sort_server_rows` と同じ列分岐を使う
/// (#31 のソート機構に Filter タブを接続する)。
pub(crate) fn sort_filter_rows(rows: &mut [FilterRow], sort: SortState) {
    let col = SortColumn::filter_at(sort.column);
    let desc = sort.descending;
    rows.sort_by(|a, b| {
        let primary = match col {
            SortColumn::Zone => cmp_str(&a.zone, &b.zone, desc),
            SortColumn::Rps => cmp_f64(a.rps, b.rps, desc),
            SortColumn::R2xx => cmp_opt_f64(a.r2xx_pct, b.r2xx_pct, desc),
            SortColumn::R4xx => cmp_opt_f64(a.r4xx_pct, b.r4xx_pct, desc),
            SortColumn::R5xx => cmp_opt_f64(a.r5xx_pct, b.r5xx_pct, desc),
            SortColumn::P95 => cmp_p95(&a.p95, &b.p95, desc),
            SortColumn::InPerSec => cmp_f64(a.bw_in_per_sec, b.bw_in_per_sec, desc),
            SortColumn::OutPerSec => cmp_f64(a.bw_out_per_sec, b.bw_out_per_sec, desc),
            // Filter タブに無い列 (STATE / Cache 系)。`filter_at` が範囲外を ZONE に
            // 倒すため実際には到達しないが、網羅性のため ZONE 名で安定ソートする。
            _ => cmp_str(&a.zone, &b.zone, desc),
        };
        primary.then_with(|| a.zone.cmp(&b.zone))
    });
}

/// cache zone の hit 率 (パーセント)。分母は
/// `hit + miss + bypass + expired + stale + updating + revalidated + scarce`。
/// 分母 0 (まだキャッシュ系応答が無い) なら `None`。
///
/// `state::derived::cache_hit_pct` と同じ算出だが、render 層は derived snapshot
/// ではなく生 snapshot から再計算する設計 (Server / Upstream と同じ) なので
/// table 層に閉じた private helper として持つ。
fn cache_hit_pct(r: &Responses) -> Option<f64> {
    let denom =
        r.hit + r.miss + r.bypass + r.expired + r.stale + r.updating + r.revalidated + r.scarce;
    if denom == 0 {
        None
    } else {
        Some(r.hit as f64 * 100.0 / denom as f64)
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

/// bytes を 1024 進・小数 1 桁で人間可読化する (`"1.2 GB"` 等)。
///
/// `header::format_bps` と同じ単位選択ロジックだが `/s` を付けない (USED 列の
/// used / max サイズ表示用)。issue #30 は USED を `humansize` で出すことを挙げて
/// いるが、本プロジェクトは既に 1024 進の inline フォーマッタ (`format_bps`) を
/// 採用済みで、新規依存を足すより既存スタイルに合わせる方が一貫する
/// (CLAUDE.md「依存」/ Karpathy 原則 2・3)。`format_bps` への共通化は header
/// (#27) への波及を伴うため本 PR の scope 外。
pub(crate) fn format_size(n: u64) -> String {
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
        format!("{:.1} GB", n as f64 / GB as f64)
    } else {
        format!("{:.1} TB", n as f64 / TB as f64)
    }
}

/// USED 列を `"4.0 KB / 8.0 MB (0%)"` のように整形する。
///
/// `max_size == 0` (cache 未設定 / max 不明) のときは割合を出せないため
/// `"used / max"` のみ (パーセント無し)。割合は整数 % に丸める。
pub(crate) fn format_used(used: u64, max: u64) -> String {
    if max == 0 {
        format!("{} / {}", format_size(used), format_size(max))
    } else {
        let pct = (used as f64 / max as f64 * 100.0).round() as u64;
        format!("{} / {} ({}%)", format_size(used), format_size(max), pct)
    }
}

/// 現在の `active_tab` + `cursor` が指す zone 名を返す (Enter で詳細を開くため)。
///
/// render と同じ「build → filter → sort」を再現した行から `cursor` 位置を引く。
/// snapshot 未取得 / 行 0 件 / 未実装タブ (Cache は #30) は `None`。
pub fn selected_zone(app: &App) -> Option<String> {
    let now = app.history.latest()?;
    let prev = app.history.previous();
    let dt_secs = match prev {
        Some(p) => (now.status.now_msec.saturating_sub(p.status.now_msec) as f64) / 1000.0,
        None => 0.0,
    };
    let prev_status = prev.map(|p| &p.status);
    match app.active_tab {
        Tab::Server => {
            let mut rows = build_server_rows(&now.status, prev_status, dt_secs);
            retain_matching(&mut rows, &app.filter, |r| r.zone.as_str());
            sort_server_rows(&mut rows, app.sort);
            let idx = app.cursor.min(rows.len().checked_sub(1)?);
            Some(rows[idx].zone.clone())
        }
        Tab::Upstream => {
            let mut rows = build_upstream_rows(&now.status, prev_status, dt_secs);
            retain_matching(&mut rows, &app.filter, |r| r.zone.as_str());
            sort_upstream_rows(&mut rows, app.sort);
            let idx = app.cursor.min(rows.len().checked_sub(1)?);
            Some(rows[idx].zone.clone())
        }
        Tab::Filter => {
            let mut rows = build_filter_rows(&now.status, prev_status, dt_secs);
            retain_matching(&mut rows, &app.filter, |r| r.zone.as_str());
            sort_filter_rows(&mut rows, app.sort);
            let idx = app.cursor.min(rows.len().checked_sub(1)?);
            Some(rows[idx].zone.clone())
        }
        Tab::Cache => {
            let mut rows = build_cache_rows(&now.status, prev_status, dt_secs);
            retain_matching(&mut rows, &app.filter, |r| r.zone.as_str());
            sort_cache_rows(&mut rows, app.sort);
            let idx = app.cursor.min(rows.len().checked_sub(1)?);
            Some(rows[idx].zone.clone())
        }
    }
}

// ---------- アラート判定 (issue #47) ----------

/// `PercentileResult` から閾値比較に使う ms 値を取り出す。
///
/// `Value` / `Average` (histogram なし fallback) はその ms、`Overflow(max)` は
/// p95 が最終 bucket 境界を超えている = 少なくとも `max` ms なので `max`。
/// `NoData` は判定不能なので `None`。
fn p95_ms_value(p: PercentileResult) -> Option<f64> {
    match p {
        PercentileResult::Value(ms) | PercentileResult::Average(ms) => Some(ms),
        PercentileResult::Overflow(max) => Some(max as f64),
        PercentileResult::NoData => None,
    }
}

/// 行が `cfg` の p95 閾値以上か (= アラート対象か)。
///
/// p95(ms) が閾値以上なら `true`。閾値未設定 (`None`) や p95 が `NoData` のときは
/// 判定に寄与しない。
pub(crate) fn row_is_alerting(p95: PercentileResult, cfg: &AlertConfig) -> bool {
    if let (Some(thr), Some(ms)) = (cfg.max_p95_ms, p95_ms_value(p95)) {
        if ms >= thr as f64 {
            return true;
        }
    }
    false
}

/// 最新 snapshot で Server / Upstream / Filter のいずれかにアラート行があるか。
///
/// ベル発火 (`main.rs`) の判定に使う。閾値未設定なら常に `false`。現在の
/// `active_tab` に関わらず全タブを評価する (別タブを見ていてもアラートを
/// 取りこぼさないため)。Filter タブの各 key は serverZones と同形で 5xx / p95 を
/// 持つので対象に含める。Cache タブは 5xx / p95 を持たないので対象外。
pub fn any_row_alerting(app: &App) -> bool {
    if !app.alerts.is_enabled() {
        return false;
    }
    let Some(now) = app.history.latest() else {
        return false;
    };
    let prev = app.history.previous();
    let dt_secs = match prev {
        Some(p) => (now.status.now_msec.saturating_sub(p.status.now_msec) as f64) / 1000.0,
        None => 0.0,
    };
    let prev_status = prev.map(|p| &p.status);
    let server = build_server_rows(&now.status, prev_status, dt_secs)
        .iter()
        .any(|r| row_is_alerting(r.p95, &app.alerts));
    let upstream = build_upstream_rows(&now.status, prev_status, dt_secs)
        .iter()
        .any(|r| row_is_alerting(r.p95, &app.alerts));
    let filter = build_filter_rows(&now.status, prev_status, dt_secs)
        .iter()
        .any(|r| row_is_alerting(r.p95, &app.alerts));
    server || upstream || filter
}

// ---------- render ----------

/// 現在の `active_tab` に応じて Server / Upstream / Cache を描画する (issue #150)。
///
/// 副作用:
/// - `app.visible_rows` を「表示行数」で更新 (cursor 上限算出用)。
/// - `app.page_size` を「body 高さ」で更新 (PgUp/PgDn の移動量)。
pub fn render(f: &mut Frame<'_>, app: &App, area: Rect) {
    match app.active_tab {
        Tab::Server => render_server(f, app, area),
        Tab::Upstream => render_upstream(f, app, area),
        Tab::Cache => render_cache(f, app, area),
        Tab::Filter => render_filter(f, app, area),
    }
}

/// タブ名 (Tab → 表示ラベル) のマッピング (issue #150)。
fn tab_label(tab: Tab) -> &'static str {
    match tab {
        Tab::Server => "Server Zones",
        Tab::Upstream => "Upstream Servers",
        Tab::Cache => "Cache Zones",
        Tab::Filter => "Filter Zones",
    }
}

/// タブごとの box title (`Tab名 · count [· filter "q"]`) を構築する (issue #150)。
/// `total` はフィルタ適用前の総行数、`visible` は適用後。`filter` が空なら省略。
fn build_box_title(tab: Tab, visible: usize, total: usize, filter: &str) -> String {
    let name = tab_label(tab);
    if filter.is_empty() {
        format!(" {name} · {visible} ")
    } else if visible == total {
        format!(" {name} · {visible} · filter \"{filter}\" ")
    } else {
        format!(" {name} · {visible}/{total} · filter \"{filter}\" ")
    }
}

/// アクティブソート列の見出しに `↓` / `↑` (mono: `v` / `^`) を suffix する。
fn header_cell_with_sort<'a>(
    label: &'a str,
    col_index: u8,
    sort: SortState,
    theme: &Theme,
) -> Cell<'a> {
    if sort.column == col_index {
        let arrow = if theme.mono {
            if sort.descending {
                "v"
            } else {
                "^"
            }
        } else if sort.descending {
            "↓"
        } else {
            "↑"
        };
        Cell::from(format!("{label}{arrow}")).style(theme.table_header)
    } else {
        Cell::from(label).style(theme.table_header)
    }
}

/// indicator 列 (1 cell) のセル。カーソル行に `▶` (mono: `>`) を出す。
/// アラートは p95 列の `theme.alert` 着色で表現するため、ここでは glyph を出さない
/// (80 cols で ZONE 列の幅を確保するためのトレードオフ)。
fn indicator_cell<'a>(is_cursor: bool, _is_alerting: bool, theme: &Theme) -> Cell<'a> {
    if is_cursor {
        let cursor_glyph = if theme.mono { ">" } else { "▶" };
        Cell::from(Span::styled(cursor_glyph, theme.row_selected))
    } else {
        Cell::from(" ")
    }
}

/// table 全体を rounded box (mono: plain) で囲み、その内側 `Rect` を返す。
/// 呼び出し側は inner に対して `render_stateful_widget` する。
///
/// `Padding::right(1)` で最終列 (OUT/s 等) と右枠線の間に 1 セル分の
/// 余白を確保する (issue #152 ユーザフィードバック: 値が枠線に張り付いて
/// 読みにくいため)。
fn render_tab_box(f: &mut Frame<'_>, app: &App, area: Rect, title: String) -> Rect {
    let border_type = if app.theme.mono {
        BorderType::Plain
    } else {
        BorderType::Rounded
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(border_type)
        .border_style(app.theme.border)
        .padding(Padding::right(1))
        .title(Span::styled(title, app.theme.title));
    let inner = block.inner(area);
    f.render_widget(block, area);
    inner
}

/// 「データなし」プレースホルダ描画 (4 タブ共通)。box 込み。
fn render_no_data(f: &mut Frame<'_>, app: &App, area: Rect, tab: Tab) {
    let title = format!(" {} · (no data) ", tab_label(tab));
    let inner = render_tab_box(f, app, area, title);
    if inner.height == 0 {
        return;
    }
    let p = Paragraph::new(Line::from(Span::styled(
        "waiting for first VTS snapshot…",
        Style::default().add_modifier(Modifier::DIM),
    )));
    f.render_widget(p, inner);
    app.visible_rows.set(0);
    app.page_size
        .set(area.height.saturating_sub(3).max(1) as usize);
}

/// 数値を右寄せした文字列に整形する (cell の `Length` 幅に収まる範囲で)。
fn right_aligned(s: &str, width: usize) -> String {
    if s.chars().count() >= width {
        s.to_string()
    } else {
        format!("{s:>width$}")
    }
}

/// Upstream タブの ZONE 列の最小表示幅。80 cols ターミナルでも他列を圧迫しない
/// 下限値。`backend_api/127.0.0.1:9001` のような長い peer 名は wide 端末では
/// 全長表示し、狭い端末でのみ `truncate_middle` で中央省略する。
const UPSTREAM_ZONE_WIDTH_MIN: usize = 12;

/// Upstream の ZONE 列に割り当てられる実描画幅 (cells) を、テーブル inner 幅から
/// 逆算する。固定列 (RPS / 2xx% / ... / STATE) と列間スペーシングを引いた残りを
/// ZONE が取る、という ratatui Table の振る舞いに合わせる。
fn upstream_zone_render_width(inner_width: u16) -> usize {
    // 固定列幅合計: indicator(1) + RPS(6) + 2xx%(6) + 4xx%(6) + 5xx%(6)
    //              + p95(8) + IN/s(9) + OUT/s(9) + STATE(6) = 57
    const FIXED_SUM: u16 = 57;
    // 10 列の間に挿入される 9 個の 1 文字スペーサ。
    const SPACING: u16 = 9;
    let zone = inner_width.saturating_sub(FIXED_SUM + SPACING);
    (zone as usize).max(UPSTREAM_ZONE_WIDTH_MIN)
}

/// `s` が `width` 文字以下ならそのまま、超えるなら中央に `…` を入れて両端を残す。
///
/// upstream の zone は `group/host:port` 形式で、識別に必要なのは末尾の port なので
/// 末尾を残す中央省略を選ぶ。`width < 3` のケースは右側を切り捨てるだけにする
/// (実運用では 12 以上を想定)。
fn truncate_middle(s: &str, width: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= width {
        return s.to_string();
    }
    if width < 3 {
        return chars.into_iter().take(width).collect();
    }
    // 残りを左右に振り分け、末尾を 1 文字優先する (port 番号の末尾を残すため)。
    let keep = width - 1; // `…` の分。
    let right_keep = keep / 2 + (keep % 2);
    let left_keep = keep - right_keep;
    let mut out = String::with_capacity(width * 4);
    out.extend(chars.iter().take(left_keep));
    out.push('…');
    out.extend(chars.iter().skip(chars.len() - right_keep));
    out
}

/// Server タブを `area` に描画する (旧 `render`)。
fn render_server(f: &mut Frame<'_>, app: &App, area: Rect) {
    let latest = app.history.latest();

    // Connecting 状態: snapshot がまだ無い → box + プレースホルダ。
    let Some(now) = latest else {
        render_no_data(f, app, area, Tab::Server);
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
    let all_rows = build_server_rows(&now.status, prev.map(|p| &p.status), dt_secs);
    let total = all_rows.len();
    let mut rows = all_rows;
    retain_matching(&mut rows, &app.filter, |r| r.zone.as_str());
    sort_server_rows(&mut rows, app.sort);

    let row_count = rows.len();
    let title = build_box_title(Tab::Server, row_count, total, &app.filter);
    let inner = render_tab_box(f, app, area, title);
    if inner.height == 0 {
        return;
    }

    let theme = &app.theme;
    let header_row = Row::new(vec![
        Cell::from(""),
        header_cell_with_sort("ZONE", 0, app.sort, theme),
        header_cell_with_sort("RPS", 1, app.sort, theme),
        header_cell_with_sort("2xx%", 2, app.sort, theme),
        header_cell_with_sort("4xx%", 3, app.sort, theme),
        header_cell_with_sort("5xx%", 4, app.sort, theme),
        header_cell_with_sort("p95", 5, app.sort, theme),
        header_cell_with_sort("IN/s", 6, app.sort, theme),
        header_cell_with_sort("OUT/s", 7, app.sort, theme),
    ])
    .style(theme.table_header.add_modifier(Modifier::UNDERLINED));

    let body_rows: Vec<Row> = rows
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let alerting = row_is_alerting(r.p95, &app.alerts);
            let is_cursor = i == app.cursor;
            let r5_style = if r.r5xx_pct.is_some_and(|p| p > 0.0) {
                theme.status_err
            } else {
                Style::default()
            };
            let p95_style = if alerting {
                theme.alert
            } else {
                Style::default()
            };
            Row::new(vec![
                indicator_cell(is_cursor, alerting, theme),
                Cell::from(r.zone.clone()),
                Cell::from(right_aligned(&format_rps(r.rps), 5)),
                Cell::from(right_aligned(&format_ratio(r.r2xx_pct), 6)),
                Cell::from(right_aligned(&format_ratio(r.r4xx_pct), 6)),
                Cell::from(Span::styled(
                    right_aligned(&format_ratio(r.r5xx_pct), 6),
                    r5_style,
                )),
                Cell::from(Span::styled(
                    right_aligned(&format_p95(r.p95), 7),
                    p95_style,
                )),
                Cell::from(right_aligned(
                    &format_bps(r.bw_in_per_sec.round() as u64),
                    9,
                )),
                Cell::from(right_aligned(
                    &format_bps(r.bw_out_per_sec.round() as u64),
                    9,
                )),
            ])
        })
        .collect();

    let widths = [
        Constraint::Length(1), // indicator (cursor ▶)
        Constraint::Min(10),   // ZONE (可変。80 cols で 19 cells に伸びる)
        Constraint::Length(6), // RPS
        Constraint::Length(6), // 2xx%
        Constraint::Length(6), // 4xx%
        Constraint::Length(6), // 5xx%
        Constraint::Length(8), // p95
        Constraint::Length(9), // IN/s
        Constraint::Length(9), // OUT/s
    ];

    let table = Table::new(body_rows, widths)
        .header(header_row)
        .row_highlight_style(theme.row_selected);

    let mut state = TableState::default();
    let clamped = if row_count == 0 {
        None
    } else {
        Some(app.cursor.min(row_count - 1))
    };
    state.select(clamped);

    f.render_stateful_widget(table, inner, &mut state);

    app.visible_rows.set(row_count);
    // 「ページ」= inner 部の高さからヘッダ 1 行を引く (最低 1)。
    app.page_size
        .set(inner.height.saturating_sub(1).max(1) as usize);
}

/// Upstream タブを `area` に描画する。
///
/// 副作用は `render_server` と同じ (`app.visible_rows` / `app.page_size`)。
/// STATE 列は theme に応じて `up`/`backup`/`down` または `[U]`/`[B]`/`[D]` を
/// 出し、`backup` / `down` は theme の警告色 / エラー色で強調する。
fn render_upstream(f: &mut Frame<'_>, app: &App, area: Rect) {
    let latest = app.history.latest();

    let Some(now) = latest else {
        render_no_data(f, app, area, Tab::Upstream);
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
    let all_rows = build_upstream_rows(&now.status, prev.map(|p| &p.status), dt_secs);
    let total = all_rows.len();
    let mut rows = all_rows;
    retain_matching(&mut rows, &app.filter, |r| r.zone.as_str());
    sort_upstream_rows(&mut rows, app.sort);

    let row_count = rows.len();
    let title = build_box_title(Tab::Upstream, row_count, total, &app.filter);
    let inner = render_tab_box(f, app, area, title);
    if inner.height == 0 {
        return;
    }

    let zone_render_width = upstream_zone_render_width(inner.width);

    let theme = &app.theme;
    let mono = theme.mono;
    let header_row = Row::new(vec![
        Cell::from(""),
        header_cell_with_sort("ZONE", 0, app.sort, theme),
        header_cell_with_sort("RPS", 1, app.sort, theme),
        header_cell_with_sort("2xx%", 2, app.sort, theme),
        header_cell_with_sort("4xx%", 3, app.sort, theme),
        header_cell_with_sort("5xx%", 4, app.sort, theme),
        header_cell_with_sort("p95", 5, app.sort, theme),
        header_cell_with_sort("IN/s", 6, app.sort, theme),
        header_cell_with_sort("OUT/s", 7, app.sort, theme),
        header_cell_with_sort("STATE", 8, app.sort, theme),
    ])
    .style(theme.table_header.add_modifier(Modifier::UNDERLINED));

    let body_rows: Vec<Row> = rows
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let alerting = row_is_alerting(r.p95, &app.alerts);
            let is_cursor = i == app.cursor;
            let r5_style = if r.r5xx_pct.is_some_and(|p| p > 0.0) {
                theme.status_err
            } else {
                Style::default()
            };
            let p95_style = if alerting {
                theme.alert
            } else {
                Style::default()
            };
            let state_style = match r.state {
                UpstreamState::Down => theme.status_err,
                UpstreamState::Backup => theme.status_warn,
                UpstreamState::Up => Style::default(),
            };
            Row::new(vec![
                indicator_cell(is_cursor, alerting, theme),
                Cell::from(truncate_middle(&r.zone, zone_render_width)),
                Cell::from(right_aligned(&format_rps(r.rps), 5)),
                Cell::from(right_aligned(&format_ratio(r.r2xx_pct), 6)),
                Cell::from(right_aligned(&format_ratio(r.r4xx_pct), 6)),
                Cell::from(Span::styled(
                    right_aligned(&format_ratio(r.r5xx_pct), 6),
                    r5_style,
                )),
                Cell::from(Span::styled(
                    right_aligned(&format_p95(r.p95), 7),
                    p95_style,
                )),
                Cell::from(right_aligned(
                    &format_bps(r.bw_in_per_sec.round() as u64),
                    9,
                )),
                Cell::from(right_aligned(
                    &format_bps(r.bw_out_per_sec.round() as u64),
                    9,
                )),
                Cell::from(Span::styled(
                    format_upstream_state(r.state, mono),
                    state_style,
                )),
            ])
        })
        .collect();

    let widths = [
        Constraint::Length(1),                           // indicator
        Constraint::Min(UPSTREAM_ZONE_WIDTH_MIN as u16), // ZONE
        Constraint::Length(6),                           // RPS
        Constraint::Length(6),                           // 2xx%
        Constraint::Length(6),                           // 4xx%
        Constraint::Length(6),                           // 5xx%
        Constraint::Length(8),                           // p95
        Constraint::Length(9),                           // IN/s
        Constraint::Length(9),                           // OUT/s
        Constraint::Length(6),                           // STATE
    ];

    let table = Table::new(body_rows, widths)
        .header(header_row)
        .row_highlight_style(theme.row_selected);

    let mut state = TableState::default();
    let clamped = if row_count == 0 {
        None
    } else {
        Some(app.cursor.min(row_count - 1))
    };
    state.select(clamped);

    f.render_stateful_widget(table, inner, &mut state);

    app.visible_rows.set(row_count);
    app.page_size
        .set(inner.height.saturating_sub(1).max(1) as usize);
}

/// Cache タブを `area` に描画する。
///
/// 副作用は `render_server` と同じ (`app.visible_rows` / `app.page_size`)。
/// cacheZones は latency / status を持たないため列構成は HIT% / MISS / EXPIRED /
/// STALE / USED。HIT% が分母 0 で取れない zone は `—` 表示。
fn render_cache(f: &mut Frame<'_>, app: &App, area: Rect) {
    let latest = app.history.latest();

    let Some(now) = latest else {
        render_no_data(f, app, area, Tab::Cache);
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
    let all_rows = build_cache_rows(&now.status, prev.map(|p| &p.status), dt_secs);
    let total = all_rows.len();
    let mut rows = all_rows;
    retain_matching(&mut rows, &app.filter, |r| r.zone.as_str());
    sort_cache_rows(&mut rows, app.sort);

    let row_count = rows.len();
    let title = build_box_title(Tab::Cache, row_count, total, &app.filter);
    let inner = render_tab_box(f, app, area, title);
    if inner.height == 0 {
        return;
    }

    let theme = &app.theme;
    let header_row = Row::new(vec![
        Cell::from(""),
        header_cell_with_sort("ZONE", 0, app.sort, theme),
        header_cell_with_sort("HIT%", 1, app.sort, theme),
        header_cell_with_sort("MISS", 2, app.sort, theme),
        header_cell_with_sort("EXPIRED", 3, app.sort, theme),
        header_cell_with_sort("STALE", 4, app.sort, theme),
        header_cell_with_sort("USED", 5, app.sort, theme),
        header_cell_with_sort("IN/s", 6, app.sort, theme),
        header_cell_with_sort("OUT/s", 7, app.sort, theme),
    ])
    .style(theme.table_header.add_modifier(Modifier::UNDERLINED));

    let body_rows: Vec<Row> = rows
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let is_cursor = i == app.cursor;
            Row::new(vec![
                indicator_cell(is_cursor, false, theme),
                Cell::from(r.zone.clone()),
                Cell::from(right_aligned(&format_ratio(r.hit_pct), 6)),
                Cell::from(right_aligned(&r.miss.to_string(), 5)),
                Cell::from(right_aligned(&r.expired.to_string(), 5)),
                Cell::from(right_aligned(&r.stale.to_string(), 5)),
                Cell::from(format_used(r.used_size, r.max_size)),
                Cell::from(right_aligned(
                    &format_bps(r.bw_in_per_sec.round() as u64),
                    9,
                )),
                Cell::from(right_aligned(
                    &format_bps(r.bw_out_per_sec.round() as u64),
                    9,
                )),
            ])
        })
        .collect();

    // 80 cols (inner=78) で全列を切らずに収めるための割付。Min(10)+Min(20)+30=
    // 40 を Length 群に残す。1+6+5+5+5+9+9 = 40 でぴったり。
    // EXPIRED 列見出しは Length(5) のため `EXPIR` まで切れるが、値 (実用上は
    // 0..99999 の整数) は完全表示される。
    let widths = [
        Constraint::Length(1), // indicator
        Constraint::Min(10),   // ZONE
        Constraint::Length(6), // HIT%
        Constraint::Length(5), // MISS
        Constraint::Length(5), // EXPIRED (header `EXPIRED` は `EXPIR` に短縮)
        Constraint::Length(5), // STALE
        Constraint::Min(20),   // USED (`xx.x KB / yy.y MB (zz%)` を完全表示)
        Constraint::Length(9), // IN/s
        Constraint::Length(9), // OUT/s
    ];

    let table = Table::new(body_rows, widths)
        .header(header_row)
        .row_highlight_style(theme.row_selected);

    let mut state = TableState::default();
    let clamped = if row_count == 0 {
        None
    } else {
        Some(app.cursor.min(row_count - 1))
    };
    state.select(clamped);

    f.render_stateful_widget(table, inner, &mut state);

    app.visible_rows.set(row_count);
    app.page_size
        .set(inner.height.saturating_sub(1).max(1) as usize);
}

/// Filter タブを `area` に描画する。
///
/// 副作用は `render_server` と同じ (`app.visible_rows` / `app.page_size`)。
/// 列構成は Server タブと同一で、ZONE 列のみ `"group/key"` 表記。
fn render_filter(f: &mut Frame<'_>, app: &App, area: Rect) {
    let latest = app.history.latest();

    let Some(now) = latest else {
        render_no_data(f, app, area, Tab::Filter);
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
    let all_rows = build_filter_rows(&now.status, prev.map(|p| &p.status), dt_secs);
    let total = all_rows.len();
    let mut rows = all_rows;
    retain_matching(&mut rows, &app.filter, |r| r.zone.as_str());
    sort_filter_rows(&mut rows, app.sort);

    let row_count = rows.len();
    let title = build_box_title(Tab::Filter, row_count, total, &app.filter);
    let inner = render_tab_box(f, app, area, title);
    if inner.height == 0 {
        return;
    }

    let theme = &app.theme;
    let header_row = Row::new(vec![
        Cell::from(""),
        header_cell_with_sort("ZONE", 0, app.sort, theme),
        header_cell_with_sort("RPS", 1, app.sort, theme),
        header_cell_with_sort("2xx%", 2, app.sort, theme),
        header_cell_with_sort("4xx%", 3, app.sort, theme),
        header_cell_with_sort("5xx%", 4, app.sort, theme),
        header_cell_with_sort("p95", 5, app.sort, theme),
        header_cell_with_sort("IN/s", 6, app.sort, theme),
        header_cell_with_sort("OUT/s", 7, app.sort, theme),
    ])
    .style(theme.table_header.add_modifier(Modifier::UNDERLINED));

    let body_rows: Vec<Row> = rows
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let alerting = row_is_alerting(r.p95, &app.alerts);
            let is_cursor = i == app.cursor;
            let r5_style = if r.r5xx_pct.is_some_and(|p| p > 0.0) {
                theme.status_err
            } else {
                Style::default()
            };
            let p95_style = if alerting {
                theme.alert
            } else {
                Style::default()
            };
            Row::new(vec![
                indicator_cell(is_cursor, alerting, theme),
                Cell::from(r.zone.clone()),
                Cell::from(right_aligned(&format_rps(r.rps), 5)),
                Cell::from(right_aligned(&format_ratio(r.r2xx_pct), 6)),
                Cell::from(right_aligned(&format_ratio(r.r4xx_pct), 6)),
                Cell::from(Span::styled(
                    right_aligned(&format_ratio(r.r5xx_pct), 6),
                    r5_style,
                )),
                Cell::from(Span::styled(
                    right_aligned(&format_p95(r.p95), 7),
                    p95_style,
                )),
                Cell::from(right_aligned(
                    &format_bps(r.bw_in_per_sec.round() as u64),
                    9,
                )),
                Cell::from(right_aligned(
                    &format_bps(r.bw_out_per_sec.round() as u64),
                    9,
                )),
            ])
        })
        .collect();

    let widths = [
        Constraint::Length(1),
        Constraint::Min(10),
        Constraint::Length(6),
        Constraint::Length(6),
        Constraint::Length(6),
        Constraint::Length(6),
        Constraint::Length(8),
        Constraint::Length(9),
        Constraint::Length(9),
    ];

    let table = Table::new(body_rows, widths)
        .header(header_row)
        .row_highlight_style(theme.row_selected);

    let mut state = TableState::default();
    let clamped = if row_count == 0 {
        None
    } else {
        Some(app.cursor.min(row_count - 1))
    };
    state.select(clamped);

    f.render_stateful_widget(table, inner, &mut state);

    app.visible_rows.set(row_count);
    app.page_size
        .set(inner.height.saturating_sub(1).max(1) as usize);
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

    // ---------- 動的ソート / フィルタ (issue #31) ----------

    use crate::state::{SortColumn, SortState};

    /// 列 index を渡して sort し、ZONE 名の順を返すヘルパ。
    fn sorted_server_zones(rows: &[ServerRow], column: u8, descending: bool) -> Vec<String> {
        let mut v = rows.to_vec();
        sort_server_rows(&mut v, SortState { column, descending });
        v.iter().map(|r| r.zone.clone()).collect()
    }

    fn three_zone_status() -> (VtsStatus, VtsStatus) {
        // rps: alpha=10, beta=100, gamma=50。in/s: alpha=300, beta=100, gamma=200。
        let prev = status_with_zones(
            1000,
            &[
                ("alpha", 0, 0, 0, 0, (0, 0, 0, 0, 0), None),
                ("beta", 0, 0, 0, 0, (0, 0, 0, 0, 0), None),
                ("gamma", 0, 0, 0, 0, (0, 0, 0, 0, 0), None),
            ],
        );
        let now = status_with_zones(
            2000,
            &[
                ("alpha", 10, 300, 0, 0, (0, 10, 0, 0, 0), None),
                ("beta", 100, 100, 0, 0, (0, 100, 0, 0, 0), None),
                ("gamma", 50, 200, 0, 0, (0, 50, 0, 0, 0), None),
            ],
        );
        (prev, now)
    }

    #[test]
    fn sort_server_by_rps_desc_and_asc() {
        let (prev, now) = three_zone_status();
        let rows = build_server_rows(&now, Some(&prev), 1.0);
        // col 1 = RPS。降順: beta(100) > gamma(50) > alpha(10)。
        assert_eq!(
            sorted_server_zones(&rows, 1, true),
            vec!["beta", "gamma", "alpha"]
        );
        // 昇順: alpha(10) < gamma(50) < beta(100)。
        assert_eq!(
            sorted_server_zones(&rows, 1, false),
            vec!["alpha", "gamma", "beta"]
        );
    }

    #[test]
    fn sort_server_by_zone_name() {
        let (prev, now) = three_zone_status();
        let rows = build_server_rows(&now, Some(&prev), 1.0);
        // col 0 = ZONE。昇順: alpha, beta, gamma。
        assert_eq!(
            sorted_server_zones(&rows, 0, false),
            vec!["alpha", "beta", "gamma"]
        );
        // 降順: gamma, beta, alpha。
        assert_eq!(
            sorted_server_zones(&rows, 0, true),
            vec!["gamma", "beta", "alpha"]
        );
    }

    #[test]
    fn sort_server_by_in_per_sec() {
        let (prev, now) = three_zone_status();
        let rows = build_server_rows(&now, Some(&prev), 1.0);
        // col 6 = IN/s。降順: alpha(300) > gamma(200) > beta(100)。
        assert_eq!(
            sorted_server_zones(&rows, 6, true),
            vec!["alpha", "gamma", "beta"]
        );
    }

    #[test]
    fn sort_server_all_columns_produce_deterministic_order() {
        // 受け入れ条件「全ソート列が動作する」: Server 8 列すべてで panic せず
        // 全行が保持されること (件数不変) を確認する。
        let (prev, now) = three_zone_status();
        let rows = build_server_rows(&now, Some(&prev), 1.0);
        for col in 0u8..8 {
            for desc in [true, false] {
                let out = sorted_server_zones(&rows, col, desc);
                assert_eq!(out.len(), 3, "col {col} desc={desc} lost rows");
            }
        }
    }

    #[test]
    fn sort_server_p95_separates_histogram_and_average_groups() {
        // histogram あり zone (Value) と histogram なし zone (Average) を混在させ、
        // p95 ソートで Value 群が常に Average 群より上に来ることを確認する。
        let prev = status_with_zones(
            1000,
            &[
                (
                    "hist-low",
                    0,
                    0,
                    0,
                    0,
                    (0, 0, 0, 0, 0),
                    Some((vec![10, 50, 100], vec![0, 0, 0])),
                ),
                ("avg-high", 0, 0, 0, 9999, (0, 0, 0, 0, 0), None),
            ],
        );
        let now = status_with_zones(
            2000,
            &[
                // histogram あり: p95 は低い値 (~10-50ms)
                (
                    "hist-low",
                    100,
                    0,
                    0,
                    0,
                    (0, 100, 0, 0, 0),
                    Some((vec![10, 50, 100], vec![0, 100, 100])),
                ),
                // histogram なし: Average(9999ms) という非常に大きな平均
                ("avg-high", 100, 0, 0, 9999, (0, 100, 0, 0, 0), None),
            ],
        );
        let rows = build_server_rows(&now, Some(&prev), 1.0);
        // col 5 = p95。降順でも昇順でも、histogram 群 (hist-low) が Average 群
        // (avg-high) より **上** に来る (tier 固定)。
        let desc = sorted_server_zones(&rows, 5, true);
        assert_eq!(
            desc,
            vec!["hist-low", "avg-high"],
            "histogram group must stay above Average group even with huge avg value"
        );
        let asc = sorted_server_zones(&rows, 5, false);
        assert_eq!(
            asc,
            vec!["hist-low", "avg-high"],
            "tier order is fixed regardless of direction"
        );
    }

    #[test]
    fn sort_upstream_by_state_up_first() {
        let s = status_with_upstreams(
            1000,
            &[
                ("g", "d:1", 0, 0, 0, 0, (0, 0, 0, 0, 0), None, false, true),
                ("g", "u:1", 0, 0, 0, 0, (0, 0, 0, 0, 0), None, false, false),
                ("g", "b:1", 0, 0, 0, 0, (0, 0, 0, 0, 0), None, true, false),
            ],
        );
        let mut rows = build_upstream_rows(&s, None, 0.0);
        // col 8 = STATE。昇順 = up → backup → down (sort_rank 昇順)。
        sort_upstream_rows(
            &mut rows,
            SortState {
                column: 8,
                descending: false,
            },
        );
        let states: Vec<UpstreamState> = rows.iter().map(|r| r.state).collect();
        assert_eq!(
            states,
            vec![
                UpstreamState::Up,
                UpstreamState::Backup,
                UpstreamState::Down
            ]
        );
    }

    #[test]
    fn filter_is_case_insensitive_substring() {
        let s = status_with_zones(
            1000,
            &[
                ("api-gateway", 0, 0, 0, 0, (0, 0, 0, 0, 0), None),
                ("static-assets", 0, 0, 0, 0, (0, 0, 0, 0, 0), None),
                ("API-internal", 0, 0, 0, 0, (0, 0, 0, 0, 0), None),
            ],
        );
        let mut rows = build_server_rows(&s, None, 0.0);
        // 大文字 "API" でも小文字 zone "api-gateway" にマッチする。
        retain_matching(&mut rows, "API", |r| r.zone.as_str());
        let mut zones: Vec<&str> = rows.iter().map(|r| r.zone.as_str()).collect();
        zones.sort();
        assert_eq!(zones, vec!["API-internal", "api-gateway"]);
    }

    #[test]
    fn filter_empty_keeps_all_rows() {
        let s = status_with_zones(
            1000,
            &[
                ("a", 0, 0, 0, 0, (0, 0, 0, 0, 0), None),
                ("b", 0, 0, 0, 0, (0, 0, 0, 0, 0), None),
            ],
        );
        let mut rows = build_server_rows(&s, None, 0.0);
        retain_matching(&mut rows, "", |r| r.zone.as_str());
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn selected_zone_respects_filter_and_cursor() {
        // フィルタ適用後の並びで cursor が効くこと (受け入れ条件)。
        let mut app = App::new();
        app.on_fetch_ok(status_with_zones(
            1000,
            &[
                ("api-a", 0, 0, 0, 0, (0, 0, 0, 0, 0), None),
                ("static", 0, 0, 0, 0, (0, 0, 0, 0, 0), None),
                ("api-b", 0, 0, 0, 0, (0, 0, 0, 0, 0), None),
            ],
        ));
        app.filter = "api".to_string();
        // ZONE 昇順 (col 0 desc=false) で api-a, api-b。cursor=1 → api-b。
        app.sort = SortState {
            column: 0,
            descending: false,
        };
        app.cursor = 1;
        assert_eq!(selected_zone(&app).as_deref(), Some("api-b"));
        // cursor=0 → api-a。
        app.cursor = 0;
        assert_eq!(selected_zone(&app).as_deref(), Some("api-a"));
    }

    #[test]
    fn selected_zone_for_upstream_tab() {
        let mut app = App::new();
        app.active_tab = Tab::Upstream;
        app.on_fetch_ok(status_with_upstreams(
            1000,
            &[
                ("g", "a:1", 0, 0, 0, 0, (0, 0, 0, 0, 0), None, false, false),
                ("g", "b:1", 0, 0, 0, 0, (0, 0, 0, 0, 0), None, false, false),
            ],
        ));
        app.sort = SortState {
            column: 0,
            descending: false,
        };
        app.cursor = 0;
        assert_eq!(selected_zone(&app).as_deref(), Some("g/a:1"));
    }

    #[test]
    fn server_columns_label_matches_headers() {
        // 列マッピングの label が SERVER_HEADERS と一致すること (画面整合)。
        for (i, h) in SERVER_HEADERS.iter().enumerate() {
            let col = SortColumn::server_at(i as u8);
            assert_eq!(col.label(), *h, "Server col {i}");
        }
        for (i, h) in UPSTREAM_HEADERS.iter().enumerate() {
            let col = SortColumn::upstream_at(i as u8);
            assert_eq!(col.label(), *h, "Upstream col {i}");
        }
    }

    #[test]
    fn render_applies_filter_to_visible_rows() {
        let mut app = App::new();
        app.on_fetch_ok(status_with_zones(
            2000,
            &[
                ("api-gateway", 0, 0, 0, 0, (0, 0, 0, 0, 0), None),
                ("static", 0, 0, 0, 0, (0, 0, 0, 0, 0), None),
            ],
        ));
        app.filter = "api".to_string();
        let out = draw(&app, 80, 6);
        assert!(
            out.contains("api-gateway"),
            "filtered-in zone shown:\n{out}"
        );
        assert!(!out.contains("static"), "filtered-out zone hidden:\n{out}");
        // visible_rows は絞り込み後の件数 (1)。
        assert_eq!(app.visible_rows.get(), 1);
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
        let _ = draw(&app, 80, 7);
        assert_eq!(app.visible_rows.get(), 3);
        // issue #150: box border 2 + 列見出し 1 が引かれる → page_size = area - 3。
        // area=7 → inner.height=5 (border 2)、page_size = 5 - 1 (header) = 4。
        assert_eq!(app.page_size.get(), 4);
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

    // ---------- アラート判定 (issue #47) ----------

    #[test]
    fn row_is_alerting_on_p95_threshold() {
        let cfg = AlertConfig {
            max_p95_ms: Some(500),
        };
        assert!(row_is_alerting(PercentileResult::Value(500.0), &cfg)); // 境界
        assert!(row_is_alerting(PercentileResult::Average(600.0), &cfg)); // histogram なし
        assert!(row_is_alerting(PercentileResult::Overflow(500), &cfg)); // 最終 bucket 超
        assert!(!row_is_alerting(PercentileResult::Value(499.0), &cfg));
        // NoData は判定不能
        assert!(!row_is_alerting(PercentileResult::NoData, &cfg));
    }

    #[test]
    fn row_is_alerting_disabled_config_never_alerts() {
        let cfg = AlertConfig::default();
        assert!(!row_is_alerting(PercentileResult::Value(9999.0), &cfg));
    }

    #[test]
    fn any_row_alerting_detects_p95_breach_and_respects_threshold() {
        // p95=600ms の histogram を持つ zone を 1 つ作る。
        // buckets [100, 500, 1000] / counts [10, 10, 80] → p95 ≒ 950ms (bucket 1000 内)。
        let prev = status_with_zones(
            1000,
            &[(
                "z",
                100,
                0,
                0,
                0,
                (0, 100, 0, 0, 0),
                Some((vec![100, 500, 1000], vec![10, 10, 80])),
            )],
        );
        let now = status_with_zones(
            2000,
            &[(
                "z",
                200,
                0,
                0,
                0,
                (0, 200, 0, 0, 0),
                Some((vec![100, 500, 1000], vec![20, 20, 160])),
            )],
        );
        let mut app = App::new();
        app.on_fetch_ok(prev);
        app.on_fetch_ok(now);

        app.alerts = AlertConfig {
            max_p95_ms: Some(500),
        };
        assert!(any_row_alerting(&app));

        // 閾値を上に動かすと検知しない
        app.alerts = AlertConfig {
            max_p95_ms: Some(2000),
        };
        assert!(!any_row_alerting(&app));

        // 無効化すると常に false
        app.alerts = AlertConfig::default();
        assert!(!any_row_alerting(&app));
    }

    #[test]
    fn any_row_alerting_false_without_snapshot() {
        let mut app = App::new();
        app.alerts = AlertConfig {
            max_p95_ms: Some(0),
        };
        assert!(!any_row_alerting(&app));
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
        // issue #150 で indicator 列が追加されたため、ZONE 列の有効幅が狭まる。
        // ZONE 名が truncate されないだけの余裕を持たせて 130 桁で描画する。
        let out = draw_upstream(&mut app, 130, 6);
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

    // ========== Cache タブ (issue #30) ==========

    /// cache zone 1 つぶんのテスト定義 (clippy::type-complexity 回避)。
    /// `(name, max_size, used_size, in_bytes, out_bytes, hit, miss, expired, stale)`。
    /// bypass / updating / revalidated / scarce は 0 固定。
    type CacheSpec<'a> = (&'a str, u64, u64, u64, u64, u64, u64, u64, u64);

    /// `cacheZones` 入りの VtsStatus を作る。
    fn status_with_caches(now_msec: u64, zones: &[CacheSpec<'_>]) -> VtsStatus {
        let cache_zones: serde_json::Map<String, serde_json::Value> = zones
            .iter()
            .map(|(name, max, used, ib, ob, hit, miss, expired, stale)| {
                let v = serde_json::json!({
                    "maxSize": max,
                    "usedSize": used,
                    "inBytes": ib,
                    "outBytes": ob,
                    "responses": {
                        "1xx": 0, "2xx": 0, "3xx": 0, "4xx": 0, "5xx": 0,
                        "miss": miss, "bypass": 0, "expired": expired, "stale": stale,
                        "updating": 0, "revalidated": 0, "hit": hit, "scarce": 0,
                    },
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
            "cacheZones": serde_json::Value::Object(cache_zones),
        });
        serde_json::from_value(raw).unwrap()
    }

    /// `active_tab = Cache` に切り替えて描画した結果を文字列で返す。
    fn draw_cache(app: &mut App, w: u16, h: u16) -> String {
        app.active_tab = Tab::Cache;
        draw(app, w, h)
    }

    // ---------- format_size / format_used ----------

    #[test]
    fn format_size_branches_on_magnitude() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(1024), "1.0 KB");
        assert_eq!(format_size(1536), "1.5 KB");
        assert_eq!(format_size(1024 * 1024), "1.0 MB");
        assert_eq!(format_size(1024u64.pow(3)), "1.0 GB");
        assert_eq!(format_size(1024u64.pow(4)), "1.0 TB");
    }

    #[test]
    fn format_used_shows_used_max_and_percent() {
        // 2GB / 4GB = 50%
        assert_eq!(
            format_used(2 * 1024u64.pow(3), 4 * 1024u64.pow(3)),
            "2.0 GB / 4.0 GB (50%)"
        );
        // 4096B / 8MB ≈ 0%
        assert_eq!(format_used(4096, 8 * 1024 * 1024), "4.0 KB / 8.0 MB (0%)");
    }

    #[test]
    fn format_used_without_max_omits_percent() {
        // max_size == 0 のときは割合を出さない (0 除算回避)。
        assert_eq!(format_used(0, 0), "0 B / 0 B");
        assert_eq!(format_used(1024, 0), "1.0 KB / 0 B");
    }

    // ---------- cache_hit_pct ----------

    #[test]
    fn cache_hit_pct_none_when_denominator_zero() {
        assert!(cache_hit_pct(&Responses::default()).is_none());
    }

    #[test]
    fn cache_hit_pct_uses_full_denominator() {
        // hit=90, miss=5, expired=3, stale=2 → denom=100 → 90%
        let r = Responses {
            hit: 90,
            miss: 5,
            expired: 3,
            stale: 2,
            ..Responses::default()
        };
        let pct = cache_hit_pct(&r).unwrap();
        assert!((pct - 90.0).abs() < 1e-9, "expected 90.0, got {pct}");
    }

    // ---------- build_cache_rows ----------

    #[test]
    fn build_cache_rows_populates_fields_and_bw_from_diff() {
        let prev = status_with_caches(1000, &[("c", 8_388_608, 4096, 0, 0, 100, 1, 0, 0)]);
        let now = status_with_caches(2000, &[("c", 8_388_608, 8192, 1024, 4096, 199, 1, 0, 0)]);
        let rows = build_cache_rows(&now, Some(&prev), 1.0);
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!(r.zone, "c");
        // hit=199, miss=1 → denom=200 → 99.5%
        assert!(
            (r.hit_pct.unwrap() - 99.5).abs() < 1e-9,
            "hit% = {:?}",
            r.hit_pct
        );
        assert_eq!(r.miss, 1);
        assert_eq!(r.used_size, 8192);
        assert_eq!(r.max_size, 8_388_608);
        assert!((r.bw_in_per_sec - 1024.0).abs() < 1e-9);
        assert!((r.bw_out_per_sec - 4096.0).abs() < 1e-9);
    }

    #[test]
    fn build_cache_rows_initial_tick_has_zero_bw() {
        let now = status_with_caches(1000, &[("c", 1024, 512, 9999, 9999, 10, 0, 0, 0)]);
        let rows = build_cache_rows(&now, None, 0.0);
        assert_eq!(rows[0].bw_in_per_sec, 0.0);
        assert_eq!(rows[0].bw_out_per_sec, 0.0);
        // hit% は累積絶対値なので初 tick でも算出される
        assert!((rows[0].hit_pct.unwrap() - 100.0).abs() < 1e-9);
    }

    #[test]
    fn sort_cache_rows_default_is_hit_pct_desc_none_last_zone_tiebreak() {
        // zone a: 50%, zone b: 90%, zone c: denom 0 (None), zone d: 50%
        let now = status_with_caches(
            2000,
            &[
                ("a", 0, 0, 0, 0, 50, 50, 0, 0), // 50%
                ("b", 0, 0, 0, 0, 90, 10, 0, 0), // 90%
                ("c", 0, 0, 0, 0, 0, 0, 0, 0),   // None
                ("d", 0, 0, 0, 0, 5, 5, 0, 0),   // 50%
            ],
        );
        let rows = build_cache_rows(&now, None, 0.0);
        // 期待順: b(90) → a(50, tie zone昇順) → d(50) → c(None 末尾)
        let zones: Vec<&str> = rows.iter().map(|r| r.zone.as_str()).collect();
        assert_eq!(zones, vec!["b", "a", "d", "c"]);
    }

    // ---------- sort_cache_rows (issue #106) ----------

    /// 4 行 cache fixture (HIT% / MISS / EXPIRED / STALE / USED / BW を分散させる)。
    fn cache_rows_for_sort() -> Vec<CacheRow> {
        let now = status_with_caches(
            2000,
            &[
                //  name   max     used  in   out    hit  miss  exp  stale
                ("a", 1000, 100, 10, 100, 80, 20, 5, 1), // 80%, used 10%
                ("b", 1000, 800, 5, 50, 50, 50, 1, 2),   // 50%, used 80%
                ("c", 1000, 500, 20, 200, 90, 10, 0, 0), // 90%, used 50%
                ("d", 0, 0, 1, 10, 0, 0, 0, 0),          // None (分母 0), max=0
            ],
        );
        let prev = status_with_caches(
            1000,
            &[
                ("a", 1000, 100, 0, 0, 80, 20, 5, 1),
                ("b", 1000, 800, 0, 0, 50, 50, 1, 2),
                ("c", 1000, 500, 0, 0, 90, 10, 0, 0),
                ("d", 0, 0, 0, 0, 0, 0, 0, 0),
            ],
        );
        // dt = 1.0s なので bw = (in/out_bytes) そのまま
        build_cache_rows(&now, Some(&prev), 1.0)
    }

    /// `sort_cache_rows` で列指定 → 期待順を検証するための薄いヘルパ。
    fn sorted_zones(col: u8, descending: bool) -> Vec<String> {
        let mut rows = cache_rows_for_sort();
        sort_cache_rows(
            &mut rows,
            SortState {
                column: col,
                descending,
            },
        );
        rows.iter().map(|r| r.zone.clone()).collect()
    }

    #[test]
    fn sort_cache_rows_zone_column_asc_desc() {
        // key 1 = ZONE。昇順 a,b,c,d / 降順 d,c,b,a
        assert_eq!(sorted_zones(0, false), vec!["a", "b", "c", "d"]);
        assert_eq!(sorted_zones(0, true), vec!["d", "c", "b", "a"]);
    }

    #[test]
    fn sort_cache_rows_hit_pct_with_none_at_end() {
        // key 2 = HIT%。降順は c(90) > a(80) > b(50) > d(None 末尾)。
        assert_eq!(sorted_zones(1, true), vec!["c", "a", "b", "d"]);
        // 昇順は b(50) < a(80) < c(90)、None は末尾固定。
        assert_eq!(sorted_zones(1, false), vec!["b", "a", "c", "d"]);
    }

    #[test]
    fn sort_cache_rows_miss_column_descending() {
        // key 3 = MISS。値は b(50), a(20), c(10), d(0)。降順: b > a > c > d。
        assert_eq!(sorted_zones(2, true), vec!["b", "a", "c", "d"]);
    }

    #[test]
    fn sort_cache_rows_expired_column() {
        // key 4 = EXPIRED。値は a(5), b(1), c(0), d(0)。降順: a, b, c, d (c/d は tie で zone 名)。
        assert_eq!(sorted_zones(3, true), vec!["a", "b", "c", "d"]);
    }

    #[test]
    fn sort_cache_rows_stale_column() {
        // key 5 = STALE。値は b(2), a(1), c(0), d(0)。降順: b, a, c, d。
        assert_eq!(sorted_zones(4, true), vec!["b", "a", "c", "d"]);
    }

    #[test]
    fn sort_cache_rows_used_column_by_ratio_not_absolute() {
        // key 6 = USED。used_ratio: a=10%, b=80%, c=50%, d=None (max=0)。
        // 降順: b(80%) > c(50%) > a(10%) > d(None 末尾)。
        // (b は絶対 used_size が最大ではない: a=100, b=800, c=500 だが、比率では b)。
        assert_eq!(sorted_zones(5, true), vec!["b", "c", "a", "d"]);
        // 昇順では a(10%) < c(50%) < b(80%)、None は末尾固定。
        assert_eq!(sorted_zones(5, false), vec!["a", "c", "b", "d"]);
    }

    #[test]
    fn sort_cache_rows_in_per_sec_column() {
        // key 7 = IN/s。bw_in = a:10, b:5, c:20, d:1。降順: c, a, b, d。
        assert_eq!(sorted_zones(6, true), vec!["c", "a", "b", "d"]);
    }

    #[test]
    fn sort_cache_rows_out_per_sec_column() {
        // key 8 = OUT/s。bw_out = a:100, b:50, c:200, d:10。降順: c, a, b, d。
        assert_eq!(sorted_zones(7, true), vec!["c", "a", "b", "d"]);
    }

    #[test]
    fn sort_cache_rows_out_of_range_column_falls_back_to_zone() {
        // Cache タブには key 9 列が無い (cache_at が範囲外を ZONE に倒すフォールバック)。
        // 結果は ZONE 列ソートと同じになる。
        assert_eq!(sorted_zones(8, false), sorted_zones(0, false));
    }

    // ---------- render_cache が App::sort を反映すること (issue #106) ----------

    #[test]
    fn render_cache_reflects_app_sort_state_for_miss_column() {
        // key 3 = MISS 降順。最初の行に MISS 最大の zone が来ること。
        let mut app = App::new();
        app.on_fetch_ok(status_with_caches(
            2000,
            &[
                ("alpha", 1024, 0, 0, 0, 0, 5, 0, 0),
                ("zulu", 1024, 0, 0, 0, 0, 100, 0, 0),
            ],
        ));
        app.active_tab = Tab::Cache;
        app.sort = SortState {
            column: 2, // MISS
            descending: true,
        };
        let out = draw(&app, 100, 5);
        // ヘッダ行を除いた最初の data 行に zulu が来ること。
        let lines: Vec<&str> = out.lines().collect();
        let data_line = lines[2]; // [0]title? no actually [0]=header, [1]=zone row? let me trust the contains check
                                  // 行に zulu が alpha より前に出ていればよい (簡易版: zulu の出現 index が alpha より先)
        let zulu_pos = out.find("zulu").expect("zulu present");
        let alpha_pos = out.find("alpha").expect("alpha present");
        assert!(
            zulu_pos < alpha_pos,
            "MISS desc で zulu(100) が alpha(5) より上に来るべき:\n{out}\n(data_line: {data_line})"
        );
    }

    #[test]
    fn render_cache_applies_filter() {
        // フィルタ "demo" を入れると、demo_* のみ残ること。
        let mut app = App::new();
        app.on_fetch_ok(status_with_caches(
            2000,
            &[
                ("demo_cache", 1024, 0, 0, 0, 5, 0, 0, 0),
                ("prod_cache", 1024, 0, 0, 0, 5, 0, 0, 0),
            ],
        ));
        app.active_tab = Tab::Cache;
        app.filter = "demo".to_string();
        let out = draw(&app, 100, 5);
        assert!(out.contains("demo_cache"), "demo_cache missing:\n{out}");
        assert!(
            !out.contains("prod_cache"),
            "prod_cache should be filtered out:\n{out}"
        );
    }

    // ---------- selected_zone が Cache タブで動くこと (issue #106) ----------

    #[test]
    fn selected_zone_returns_cache_zone_at_cursor() {
        // Cache タブで HIT% 降順なら cursor=0 は最高 hit% の zone。
        let mut app = App::new();
        app.on_fetch_ok(status_with_caches(
            2000,
            &[
                ("low", 1024, 0, 0, 0, 50, 50, 0, 0), // 50%
                ("high", 1024, 0, 0, 0, 99, 1, 0, 0), // 99%
            ],
        ));
        app.active_tab = Tab::Cache;
        app.cursor = 0;
        assert_eq!(selected_zone(&app).as_deref(), Some("high"));
        app.cursor = 1;
        assert_eq!(selected_zone(&app).as_deref(), Some("low"));
    }

    // ---------- render (Cache) ----------

    #[test]
    fn render_cache_shows_headers_and_values() {
        let mut app = App::new();
        app.on_fetch_ok(status_with_caches(
            2000,
            &[("demo_cache", 8_388_608, 4096, 0, 0, 99, 1, 0, 0)],
        ));
        let out = draw_cache(&mut app, 100, 5);
        for h in &CACHE_HEADERS {
            // EXPIRED 列は 80 cols で他列を圧迫しないよう Length(5) に縮めて
            // おり、見出しは `EXPIR` まで切れる (値は完全表示)。
            let expected = if *h == "EXPIRED" { "EXPIR" } else { *h };
            assert!(
                out.contains(expected),
                "header {expected} missing in:\n{out}"
            );
        }
        assert!(out.contains("demo_cache"), "out:\n{out}");
        // hit=99, miss=1 → 99.0%
        assert!(out.contains("99.0%"), "out:\n{out}");
        // USED: 4096 / 8MB
        assert!(out.contains("4.0 KB / 8.0 MB"), "out:\n{out}");
    }

    #[test]
    fn render_cache_shows_emdash_when_no_cache_responses() {
        let mut app = App::new();
        app.on_fetch_ok(status_with_caches(
            2000,
            &[("empty_cache", 1024, 0, 0, 0, 0, 0, 0, 0)],
        ));
        let out = draw_cache(&mut app, 100, 5);
        assert!(out.contains("empty_cache"), "out:\n{out}");
        // 分母 0 → HIT% は —
        assert!(out.contains("—"), "out:\n{out}");
    }

    #[test]
    fn render_cache_with_no_snapshot_shows_placeholder() {
        let mut app = App::new();
        app.active_tab = Tab::Cache;
        let out = draw(&app, 80, 3);
        assert!(
            out.contains("waiting for first VTS snapshot"),
            "out:\n{out}"
        );
    }

    #[test]
    fn cache_fits_in_80x24() {
        let mut app = App::new();
        app.on_fetch_ok(status_with_caches(
            2000,
            &[("demo_cache", 8_388_608, 4096, 0, 0, 100, 0, 0, 0)],
        ));
        let _ = draw_cache(&mut app, 80, 24);
    }

    // ========== Filter タブ (issue #45) ==========

    /// filter key 1 つぶんのテスト定義 (clippy::type-complexity 回避)。
    /// `(group, key, request_counter, in_bytes, out_bytes, request_msec,
    /// (1xx,2xx,3xx,4xx,5xx), Some((msecs,counters)) or None)`。serverZones と
    /// 同形なので server 用 ZoneSpec とほぼ同じ構成。
    type FilterSpec<'a> = (
        &'a str,
        &'a str,
        u64,
        u64,
        u64,
        u64,
        (u64, u64, u64, u64, u64),
        Option<(Vec<u64>, Vec<u64>)>,
    );

    /// `filterZones` 入りの VtsStatus を作る。key は group ごとに集約される
    /// (キー順は BTreeMap で安定)。
    fn status_with_filters(now_msec: u64, keys: &[FilterSpec<'_>]) -> VtsStatus {
        use std::collections::BTreeMap;
        let mut groups: BTreeMap<String, serde_json::Map<String, serde_json::Value>> =
            BTreeMap::new();
        for (group, key, rc, ib, ob, rms, (r1, r2, r3, r4, r5), buckets) in keys {
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
            groups
                .entry((*group).to_string())
                .or_default()
                .insert((*key).to_string(), v);
        }
        let filter_zones: serde_json::Map<String, serde_json::Value> = groups
            .into_iter()
            .map(|(k, m)| (k, serde_json::Value::Object(m)))
            .collect();
        let raw = serde_json::json!({
            "hostName": "h", "nginxVersion": "1", "moduleVersion": "v",
            "loadMsec": 0u64, "nowMsec": now_msec,
            "connections": {
                "active": 0, "reading": 0, "writing": 0,
                "waiting": 0, "accepted": 0, "handled": 0, "requests": 0
            },
            "filterZones": serde_json::Value::Object(filter_zones),
        });
        serde_json::from_value(raw).unwrap()
    }

    /// `active_tab = Filter` に切り替えて描画した結果を文字列で返す。
    fn draw_filter(app: &mut App, w: u16, h: u16) -> String {
        app.active_tab = Tab::Filter;
        draw(app, w, h)
    }

    #[test]
    fn build_filter_rows_zone_is_group_slash_key() {
        let s = status_with_filters(
            1000,
            &[
                ("country::*", "US", 0, 0, 0, 0, (0, 0, 0, 0, 0), None),
                ("country::*", "JP", 0, 0, 0, 0, (0, 0, 0, 0, 0), None),
            ],
        );
        let rows = build_filter_rows(&s, None, 0.0);
        let zones: Vec<&str> = rows.iter().map(|r| r.zone.as_str()).collect();
        assert!(zones.contains(&"country::*/US"), "zones: {zones:?}");
        assert!(zones.contains(&"country::*/JP"), "zones: {zones:?}");
    }

    #[test]
    fn build_filter_rows_no_prev_returns_zero_rps() {
        let s = status_with_filters(
            1000,
            &[("country::*", "US", 0, 0, 0, 42, (0, 0, 0, 0, 0), None)],
        );
        let rows = build_filter_rows(&s, None, 0.0);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].rps, 0.0);
        // histogram 無し key は average_fallback で値を返す
        assert!(matches!(rows[0].p95, PercentileResult::Average(v) if (v - 42.0).abs() < 1e-9));
    }

    #[test]
    fn build_filter_rows_computes_rps_and_ratios_from_diff() {
        let prev = status_with_filters(
            1000,
            &[("country::*", "US", 100, 0, 0, 0, (0, 80, 0, 10, 10), None)],
        );
        let now = status_with_filters(
            2000,
            &[(
                "country::*",
                "US",
                200,
                1024,
                4096,
                0,
                (0, 180, 0, 10, 10),
                None,
            )],
        );
        let rows = build_filter_rows(&now, Some(&prev), 1.0);
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert!((r.rps - 100.0).abs() < 1e-9, "rps = {}", r.rps);
        // 差分: 2xx=100, total=100 → 2xx=100%
        assert!((r.r2xx_pct.unwrap() - 100.0).abs() < 1e-9);
        assert_eq!(r.r4xx_pct, Some(0.0));
        assert_eq!(r.r5xx_pct, Some(0.0));
        assert!((r.bw_in_per_sec - 1024.0).abs() < 1e-9);
        assert!((r.bw_out_per_sec - 4096.0).abs() < 1e-9);
    }

    #[test]
    fn build_filter_rows_default_sort_is_rps_desc_with_zone_tiebreak() {
        let now = status_with_filters(
            2000,
            &[
                ("country::*", "alpha", 100, 0, 0, 0, (0, 90, 0, 5, 5), None),
                (
                    "country::*",
                    "beta",
                    1000,
                    0,
                    0,
                    0,
                    (0, 1000, 0, 0, 0),
                    None,
                ),
                ("country::*", "gamma", 100, 0, 0, 0, (0, 90, 0, 5, 5), None),
            ],
        );
        let prev = status_with_filters(
            1000,
            &[
                ("country::*", "alpha", 50, 0, 0, 0, (0, 45, 0, 3, 2), None),
                ("country::*", "beta", 500, 0, 0, 0, (0, 500, 0, 0, 0), None),
                ("country::*", "gamma", 50, 0, 0, 0, (0, 45, 0, 3, 2), None),
            ],
        );
        let rows = build_filter_rows(&now, Some(&prev), 1.0);
        // 期待順: beta (500 rps) → alpha (50 rps, tie で zone alphabetical) → gamma
        assert_eq!(rows[0].zone, "country::*/beta");
        assert_eq!(rows[1].zone, "country::*/alpha");
        assert_eq!(rows[2].zone, "country::*/gamma");
    }

    #[test]
    fn render_filter_shows_headers_and_group_slash_key() {
        let mut app = App::new();
        app.on_fetch_ok(status_with_filters(
            2000,
            &[("country::*", "US", 100, 0, 0, 50, (0, 100, 0, 0, 0), None)],
        ));
        let out = draw_filter(&mut app, 100, 5);
        for h in &FILTER_HEADERS {
            assert!(out.contains(h), "header {h} missing in:\n{out}");
        }
        assert!(out.contains("country::*/US"), "out:\n{out}");
        // histogram 無し key は ~Nms (Average) 表示
        assert!(out.contains("~50ms"), "out:\n{out}");
    }

    #[test]
    fn render_filter_with_no_snapshot_shows_placeholder() {
        let mut app = App::new();
        app.active_tab = Tab::Filter;
        let out = draw(&app, 80, 3);
        assert!(
            out.contains("waiting for first VTS snapshot"),
            "out:\n{out}"
        );
    }

    #[test]
    fn filter_fits_in_80x24() {
        let mut app = App::new();
        app.on_fetch_ok(status_with_filters(
            2000,
            &[("country::*", "US", 0, 0, 0, 0, (0, 0, 0, 0, 0), None)],
        ));
        let _ = draw_filter(&mut app, 80, 24);
    }

    #[test]
    fn sort_filter_rows_honors_sort_state_like_server() {
        // Filter タブが #31 のソート機構に接続されていること: 列構成は Server と
        // 同一なので、ZONE 昇順 (col 0, asc) で並べ替えできる。
        let now = status_with_filters(
            2000,
            &[
                (
                    "country::*",
                    "beta",
                    1000,
                    0,
                    0,
                    0,
                    (0, 1000, 0, 0, 0),
                    None,
                ),
                ("country::*", "alpha", 10, 0, 0, 0, (0, 10, 0, 0, 0), None),
                ("country::*", "gamma", 100, 0, 0, 0, (0, 100, 0, 0, 0), None),
            ],
        );
        let mut rows = build_filter_rows(&now, None, 0.0);
        sort_filter_rows(
            &mut rows,
            SortState {
                column: 0, // ZONE
                descending: false,
            },
        );
        let zones: Vec<&str> = rows.iter().map(|r| r.zone.as_str()).collect();
        assert_eq!(
            zones,
            vec!["country::*/alpha", "country::*/beta", "country::*/gamma"]
        );
    }

    #[test]
    fn render_filter_applies_name_filter_and_sort() {
        // F4/`/` の name フィルタと 1-9/F5 のソートが Filter タブにも効くこと。
        let mut app = App::new();
        app.on_fetch_ok(status_with_filters(
            2000,
            &[
                ("country::*", "US", 0, 0, 0, 0, (0, 0, 0, 0, 0), None),
                ("method::*", "GET", 0, 0, 0, 0, (0, 0, 0, 0, 0), None),
            ],
        ));
        app.active_tab = Tab::Filter;
        // ZONE 昇順 + "method" で絞り込む。
        app.sort = SortState {
            column: 0,
            descending: false,
        };
        app.filter = "method".to_string();
        let out = draw(&app, 100, 6);
        assert!(out.contains("method::*/GET"), "out:\n{out}");
        assert!(!out.contains("country::*/US"), "filtered out:\n{out}");
        // フィルタ適用後の可視行数が 1 行 (cursor 上限算出に使う副作用)。
        assert_eq!(app.visible_rows.get(), 1);
    }

    #[test]
    fn selected_zone_filter_tab_respects_filter_and_cursor() {
        let mut app = App::new();
        app.on_fetch_ok(status_with_filters(
            2000,
            &[
                ("country::*", "US", 0, 0, 0, 0, (0, 0, 0, 0, 0), None),
                ("country::*", "JP", 0, 0, 0, 0, (0, 0, 0, 0, 0), None),
                ("method::*", "GET", 0, 0, 0, 0, (0, 0, 0, 0, 0), None),
            ],
        ));
        app.active_tab = Tab::Filter;
        app.sort = SortState {
            column: 0, // ZONE 昇順で決定的に
            descending: false,
        };
        app.filter = "country".to_string();
        // 絞り込み後の並びは "country::*/JP", "country::*/US"。cursor=1 → US。
        app.cursor = 1;
        assert_eq!(selected_zone(&app).as_deref(), Some("country::*/US"));
    }
}
