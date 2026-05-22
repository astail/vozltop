//! アプリケーション状態 (App) と派生メトリクス。
//!
//! 本 PR (issue #19) では `connection` サブモジュールに、fetch 結果を
//! 受けてバナー (Running / Stale / Disconnected) を切り替え、nginx 再起動を
//! 検出する最小の `App` を実装する。
//!
//! 後続 issue:
//! - issue #20: rolling 120 snapshot の `History` を追加
//! - issue #21-23: `derived` サブモジュールで RPS / BW / percentile を計算

pub mod connection;

pub use connection::{App, BannerStatus};
