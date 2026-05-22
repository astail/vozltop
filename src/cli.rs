//! CLI 引数の保持。
//!
//! 本 PR (issue #17) ではまだ `clap` を導入せず、`VtsClient::new` に渡せる
//! 最小フィールドだけを定義する placeholder。issue #18 で認証フラグを、
//! issue #34 で `clap::Parser` derive と全フラグを追加する。
//!
//! このような小ステップを踏むのは、issue #17 単体での acceptance criteria
//! (`cargo run -- URL` で 1 回 fetch) を成立させつつ、後続 issue で衝突を
//! 起こしにくくする (= フィールド追加だけで済むようにする) ため。

use url::Url;

/// 後続 issue で拡張される CLI 引数。
#[derive(Debug, Clone)]
pub struct Args {
    /// nginx-vts の `/status/format/json` などを指す絶対 URL。
    pub url: Url,
}
