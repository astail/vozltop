//! ソート列の定義と、タブごとの「数字キー (1-9) → 列」マッピング (issue #31)。
//!
//! タブによって表示列が異なるため、数字キーが指す列はタブごとに固定する
//! (`docs/DESIGN.md`「ソート列マッピング」)。タブ切替で同じキーが別の列を指す
//! 副作用は許容し、footer に現在のソート列名を常時表示してユーザーに認知させる。
//!
//! ## 設計
//!
//! - [`SortColumn`]: 列の **意味** を表す enum。p95 だけは histogram 群 / Average 群の
//!   分離 (`state::percentile::compare_for_sort`) が必要なので、ソート実装側で
//!   分岐するための単一の真実の源 (single source of truth)。
//! - [`server_columns`] / [`upstream_columns`] / [`cache_columns`]: 各タブの 0-based
//!   列 index → [`SortColumn`] のマッピング **テーブル**。`Tab × Key → Column` は
//!   `column_for_key` がこのテーブルを引くことで解決する (ハードコードしない)。
//!
//! Cache タブの列マッピングは定数として定義するが、行ソートの実装自体は
//! Cache タブ描画 (issue #30) 待ち。本 issue では Server / Upstream の動作を優先する。

use crate::state::Tab;

/// ソート対象の列 (意味ベース)。タブ間で共有する列 (ZONE / RPS / p95 / IN/s /
/// OUT/s) は同じ variant を使い回す。タブ固有の列のみ専用 variant を持つ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortColumn {
    /// zone 名 (Server: zone / Upstream: `group/host:port` / Cache: cache zone)。
    Zone,
    /// requests/sec。
    Rps,
    /// 2xx 応答比率。
    R2xx,
    /// 4xx 応答比率。
    R4xx,
    /// 5xx 応答比率。
    R5xx,
    /// p95 latency (histogram 群 → Average 群 → NoData 群で分離ソート)。
    P95,
    /// inbound bytes/sec。
    InPerSec,
    /// outbound bytes/sec。
    OutPerSec,
    /// Upstream 専用: down / backup / up の状態 (up 先頭)。
    State,
    /// Cache 専用: hit 率。
    HitPct,
    /// Cache 専用: miss カウント。
    Miss,
    /// Cache 専用: expired カウント。
    Expired,
    /// Cache 専用: stale カウント。
    Stale,
    /// Cache 専用: used_size / max_size。
    Used,
}

impl SortColumn {
    /// Server タブの 0-based 列 index → [`SortColumn`]。範囲外は ZONE に倒す
    /// (`apply_sort_key` が範囲外キーを弾くため通常は到達しない防御的フォールバック)。
    pub fn server_at(column: u8) -> SortColumn {
        column_at(Tab::Server, column).unwrap_or(SortColumn::Zone)
    }

    /// Upstream タブの 0-based 列 index → [`SortColumn`]。範囲外は ZONE
    /// (`server_at` と同じく防御的フォールバック)。
    pub fn upstream_at(column: u8) -> SortColumn {
        column_at(Tab::Upstream, column).unwrap_or(SortColumn::Zone)
    }

    /// footer / help 表示用の列名。`ui::table::SERVER_HEADERS` 等のヘッダ文字列と
    /// 一致させる (ユーザーが画面上のヘッダと footer の対応を取れるように)。
    pub fn label(self) -> &'static str {
        match self {
            SortColumn::Zone => "ZONE",
            SortColumn::Rps => "RPS",
            SortColumn::R2xx => "2xx%",
            SortColumn::R4xx => "4xx%",
            SortColumn::R5xx => "5xx%",
            SortColumn::P95 => "p95",
            SortColumn::InPerSec => "IN/s",
            SortColumn::OutPerSec => "OUT/s",
            SortColumn::State => "STATE",
            SortColumn::HitPct => "HIT%",
            SortColumn::Miss => "MISS",
            SortColumn::Expired => "EXPIRED",
            SortColumn::Stale => "STALE",
            SortColumn::Used => "USED",
        }
    }
}

/// Server タブの 0-based 列 index → [`SortColumn`] (8 列)。
/// 数字キー 1-8 がそれぞれ index 0-7 に対応する。
pub const SERVER_COLUMNS: [SortColumn; 8] = [
    SortColumn::Zone,
    SortColumn::Rps,
    SortColumn::R2xx,
    SortColumn::R4xx,
    SortColumn::R5xx,
    SortColumn::P95,
    SortColumn::InPerSec,
    SortColumn::OutPerSec,
];

/// Upstream タブの 0-based 列 index → [`SortColumn`] (9 列)。Server + STATE。
pub const UPSTREAM_COLUMNS: [SortColumn; 9] = [
    SortColumn::Zone,
    SortColumn::Rps,
    SortColumn::R2xx,
    SortColumn::R4xx,
    SortColumn::R5xx,
    SortColumn::P95,
    SortColumn::InPerSec,
    SortColumn::OutPerSec,
    SortColumn::State,
];

/// Cache タブの 0-based 列 index → [`SortColumn`] (8 列)。
///
/// 列マッピングは定義するが、行ソートの実装は Cache タブ描画 (issue #30) 待ち。
pub const CACHE_COLUMNS: [SortColumn; 8] = [
    SortColumn::Zone,
    SortColumn::HitPct,
    SortColumn::Miss,
    SortColumn::Expired,
    SortColumn::Stale,
    SortColumn::Used,
    SortColumn::InPerSec,
    SortColumn::OutPerSec,
];

/// 指定タブの列マッピングテーブルを返す。
pub fn columns(tab: Tab) -> &'static [SortColumn] {
    match tab {
        Tab::Server => &SERVER_COLUMNS,
        Tab::Upstream => &UPSTREAM_COLUMNS,
        Tab::Cache => &CACHE_COLUMNS,
    }
}

/// 指定タブ・0-based 列 index に対応する [`SortColumn`] を返す。
/// 範囲外の index は `None` (= そのタブには無い列)。
pub fn column_at(tab: Tab, column: u8) -> Option<SortColumn> {
    columns(tab).get(column as usize).copied()
}

/// 指定タブ・0-based 列 index の表示ラベルを返す。範囲外は `"?"`。
pub fn column_label(tab: Tab, column: u8) -> &'static str {
    column_at(tab, column).map_or("?", SortColumn::label)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_key_mapping_matches_design_table() {
        // docs/DESIGN.md「ソート列マッピング」Server タブ:
        // 1=ZONE 2=RPS 3=2xx% 4=4xx% 5=5xx% 6=p95 7=IN/s 8=OUT/s
        let expected = [
            (0, SortColumn::Zone),
            (1, SortColumn::Rps),
            (2, SortColumn::R2xx),
            (3, SortColumn::R4xx),
            (4, SortColumn::R5xx),
            (5, SortColumn::P95),
            (6, SortColumn::InPerSec),
            (7, SortColumn::OutPerSec),
        ];
        for (idx, col) in expected {
            assert_eq!(column_at(Tab::Server, idx), Some(col), "Server col {idx}");
        }
        // 9 列目 (index 8) は Server には無い
        assert_eq!(column_at(Tab::Server, 8), None);
    }

    #[test]
    fn upstream_key_mapping_matches_design_table() {
        // 1-8 は Server と同じ、9=STATE
        assert_eq!(column_at(Tab::Upstream, 0), Some(SortColumn::Zone));
        assert_eq!(column_at(Tab::Upstream, 1), Some(SortColumn::Rps));
        assert_eq!(column_at(Tab::Upstream, 8), Some(SortColumn::State));
        // 10 列目は無い
        assert_eq!(column_at(Tab::Upstream, 9), None);
    }

    #[test]
    fn cache_key_mapping_matches_design_table() {
        // 1=ZONE 2=HIT% 3=MISS 4=EXPIRED 5=STALE 6=USED 7=IN/s 8=OUT/s
        let expected = [
            (0, SortColumn::Zone),
            (1, SortColumn::HitPct),
            (2, SortColumn::Miss),
            (3, SortColumn::Expired),
            (4, SortColumn::Stale),
            (5, SortColumn::Used),
            (6, SortColumn::InPerSec),
            (7, SortColumn::OutPerSec),
        ];
        for (idx, col) in expected {
            assert_eq!(column_at(Tab::Cache, idx), Some(col), "Cache col {idx}");
        }
    }

    #[test]
    fn each_tab_column_count_matches_headers() {
        assert_eq!(columns(Tab::Server).len(), 8);
        assert_eq!(columns(Tab::Upstream).len(), 9);
        assert_eq!(columns(Tab::Cache).len(), 8);
    }

    #[test]
    fn column_label_for_zero_is_zone_all_tabs() {
        for tab in [Tab::Server, Tab::Upstream, Tab::Cache] {
            assert_eq!(column_label(tab, 0), "ZONE");
        }
    }

    #[test]
    fn column_label_out_of_range_is_question_mark() {
        assert_eq!(column_label(Tab::Server, 8), "?");
        assert_eq!(column_label(Tab::Upstream, 9), "?");
    }

    #[test]
    fn labels_match_table_headers_for_shared_columns() {
        // footer のラベルが table ヘッダと一致すること (ユーザーが対応を取れる)。
        assert_eq!(SortColumn::Rps.label(), "RPS");
        assert_eq!(SortColumn::P95.label(), "p95");
        assert_eq!(SortColumn::State.label(), "STATE");
        assert_eq!(SortColumn::HitPct.label(), "HIT%");
    }
}
