//! vozltop のライブラリクレート。
//!
//! `src/main.rs` のバイナリエントリと `tests/*.rs` の統合テストの双方から
//! 共通の型・ロジックを参照できるように `lib.rs` を分離している。

pub mod cli;
pub mod client;
pub mod model;
pub mod state;
pub mod theme;

/// テスト同期用のユーティリティ。
///
/// `NO_COLOR` のような process-wide env を読み書きするテストは、`cargo test`
/// の並列実行下で互いに干渉する。`cli::tests` と `theme::tests` の双方が同じ
/// env を触るため、共通の Mutex をここに置いて全テスト binary 内で共有する。
#[cfg(test)]
pub(crate) mod test_util {
    use std::sync::Mutex;

    /// `NO_COLOR` を含む env を変更するテストで、必ず先にロックすること。
    pub static ENV_LOCK: Mutex<()> = Mutex::new(());
}
