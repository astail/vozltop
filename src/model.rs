//! `nginx-module-vts` の `/status/format/json` レスポンスを表現する型。
//!
//! 設計は [`docs/DESIGN.md`](../../docs/DESIGN.md) の「データモデル (model.rs)」
//! 節をそのまま落としたもの。`tests/deserialize.rs` で実フィクスチャに対する
//! デコードを担保している。

use std::collections::HashMap;

use serde::Deserialize;

/// `/status/format/json` のトップレベル。
#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct VtsStatus {
    pub host_name: String,
    pub nginx_version: String,
    /// `moduleVersion` は nginx-module-vts v0.1.16 以降で追加されたフィールド。
    /// それ以前のビルドや独自パッチを当てた nginx-vts では省略されることが
    /// あり、fetch ループを 1 件で死なせないために `#[serde(default)]` で
    /// 受ける (省略時は空文字列)。
    #[serde(default)]
    pub module_version: String,
    pub load_msec: u64,
    pub now_msec: u64,
    pub connections: Connections,
    #[serde(default)]
    pub server_zones: HashMap<String, ServerZone>,
    #[serde(default)]
    pub upstream_zones: HashMap<String, Vec<UpstreamServer>>,
    #[serde(default)]
    pub cache_zones: HashMap<String, CacheZone>,
    #[serde(default)]
    pub shared_zones: SharedZones,
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct Connections {
    pub active: u64,
    pub reading: u64,
    pub writing: u64,
    pub waiting: u64,
    pub accepted: u64,
    pub handled: u64,
    pub requests: u64,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ServerZone {
    pub request_counter: u64,
    pub in_bytes: u64,
    pub out_bytes: u64,
    pub responses: Responses,
    pub request_msec_counter: u64,
    pub request_msec: u64,
    #[serde(default)]
    pub request_buckets: Option<Buckets>,
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct Responses {
    #[serde(rename = "1xx", default)]
    pub r1xx: u64,
    #[serde(rename = "2xx", default)]
    pub r2xx: u64,
    #[serde(rename = "3xx", default)]
    pub r3xx: u64,
    #[serde(rename = "4xx", default)]
    pub r4xx: u64,
    #[serde(rename = "5xx", default)]
    pub r5xx: u64,
    #[serde(default)]
    pub miss: u64,
    #[serde(default)]
    pub bypass: u64,
    #[serde(default)]
    pub expired: u64,
    #[serde(default)]
    pub stale: u64,
    #[serde(default)]
    pub updating: u64,
    #[serde(default)]
    pub revalidated: u64,
    #[serde(default)]
    pub hit: u64,
    #[serde(default)]
    pub scarce: u64,
}

/// `requestBuckets` / `responseBuckets`。
///
/// vts は秒で設定したバケツ境界を整数ミリ秒で出力する。実フィクスチャでは
/// histogram 未設定の zone でも `requestBuckets: {msecs:[], counters:[]}` の
/// 形で常に存在するため、`is_none()` チェックではなく `msecs.is_empty()` で
/// 「histogram が事実上設定されているか」を判定する側で扱う。`Option` でラップ
/// しているのは将来 nginx-vts 側がフィールドごと省略する変更を入れたケースへの
/// 安全マージン。下流で `Option<Buckets>` を扱う際は「`None` も `Some(empty)` も
/// histogram なし」として同じ分岐に倒すこと。
#[derive(Deserialize, Debug, Clone)]
pub struct Buckets {
    pub msecs: Vec<u64>,
    pub counters: Vec<u64>,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct UpstreamServer {
    /// `"10.0.0.1:8080"` 形式。
    pub server: String,
    pub request_counter: u64,
    pub in_bytes: u64,
    pub out_bytes: u64,
    pub responses: Responses,
    pub request_msec_counter: u64,
    pub request_msec: u64,
    #[serde(default)]
    pub request_buckets: Option<Buckets>,
    pub response_msec_counter: u64,
    pub response_msec: u64,
    #[serde(default)]
    pub response_buckets: Option<Buckets>,
    // 注意: 以下 5 フィールドの nginx 側 runtime default (`weight=1`,
    // `max_fails=1`, `fail_timeout=10` 秒, `backup=false`, `down=false`) と
    // Rust の `Default::default()` (`0` / `false`) は意味が異なる。実フィクスチャ
    // では nginx-module-vts が常時出力するため `#[serde(default)]` が発火する
    // のは古い nginx-vts (< v0.1.x 系) や独自ビルド省略時に限られる。表示・
    // 障害判定で 0 を「省略由来か実値か」区別したくなったら `Option<u64>` 化を
    // 検討する (v1 では区別不要と判断)。
    #[serde(default)]
    pub weight: u64,
    #[serde(default)]
    pub max_fails: u64,
    #[serde(default)]
    pub fail_timeout: u64,
    #[serde(default)]
    pub backup: bool,
    #[serde(default)]
    pub down: bool,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CacheZone {
    pub max_size: u64,
    pub used_size: u64,
    pub in_bytes: u64,
    pub out_bytes: u64,
    /// `hit` / `miss` / `bypass` / `expired` / `stale` / `updating` / `revalidated` / `scarce`
    /// を含む。1xx〜5xx は cache zone では 0 のまま (フィールド自体は省略される)。
    pub responses: Responses,
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct SharedZones {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub max_size: u64,
    #[serde(default)]
    pub used_size: u64,
    #[serde(default)]
    pub used_node: u64,
}
