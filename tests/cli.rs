//! `Args` の clap derive を経由した end-to-end の引数パースをテストする
//! (issue #37)。
//!
//! ## なぜ統合テストか
//!
//! `src/cli.rs::tests` 配下の inline テストは個別の value parser
//! (`parse_user` / `parse_header` / `parse_interval`) を直接呼ぶ単体テスト
//! が主で、clap の `derive(Parser)` 経由でこれらが正しく繋がっているかは
//! 一部しか確認していない。本ファイルは「`Args::try_parse_from(argv)` に
//! 渡したら何が返るか」という **CLI の表面契約** をロックするための回帰
//! テスト。
//!
//! issue #37 のチェックリストのうち、本 PR でカバーするのは:
//!
//! - `--user a:b`
//! - `--header 'X: Y'` (繰り返し可)
//! - `--interval 0.5`
//! - `--insecure`
//! - `--no-color`
//! - `NO_COLOR=1` 環境変数経由の効果
//!
//! sort / filter モジュールは issue #31 で実装され、対応するテストはその
//! PR に同梱する。

use std::sync::Mutex;

use clap::Parser;
use pretty_assertions::assert_eq;
use vozltop::cli::Args;

/// `NO_COLOR` 環境変数を触るテストの直列化用ロック。
///
/// `std::env::set_var` はプロセスグローバルなので、`NO_COLOR` を読む
/// `Args::no_color_effective()` をテストする際は必ずこのロックを取って
/// から env を変更し、Drop で復元する。
///
/// 各 `tests/*.rs` は独立した test binary になるため、`src/lib.rs::test_util`
/// 側の `ENV_LOCK` とは別プロセスに住む (相互に干渉しない)。
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// `NO_COLOR` を一時的に変更し、Drop で元に戻す guard。
struct EnvGuard {
    key: &'static str,
    original: Option<std::ffi::OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let original = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, original }
    }
    fn remove(key: &'static str) -> Self {
        let original = std::env::var_os(key);
        std::env::remove_var(key);
        Self { key, original }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.original {
            Some(v) => std::env::set_var(self.key, v),
            None => std::env::remove_var(self.key),
        }
    }
}

/// テスト用に `Args::try_parse_from` を呼ぶラッパ。
///
/// 第 0 要素 (binary 名) を毎度書きたくないのでここで補う。
fn parse(args: &[&str]) -> Result<Args, clap::Error> {
    let mut v = vec!["vozltop"];
    v.extend_from_slice(args);
    Args::try_parse_from(v)
}

// ---------- --user ----------

#[test]
fn parses_user_a_b() {
    let args = parse(&["http://localhost:8080/s", "-u", "alice:bob"]).unwrap();
    assert_eq!(
        args.user.as_ref().map(|(u, p)| (u.as_str(), p.as_str())),
        Some(("alice", "bob"))
    );
}

#[test]
fn parses_user_long_form() {
    let args = parse(&["http://localhost:8080/s", "--user", "alice:bob"]).unwrap();
    assert_eq!(
        args.user.as_ref().map(|(u, p)| (u.as_str(), p.as_str())),
        Some(("alice", "bob"))
    );
}

#[test]
fn user_default_is_none() {
    let args = parse(&["http://localhost:8080/s"]).unwrap();
    assert!(args.user.is_none());
}

#[test]
fn rejects_user_without_colon() {
    // clap value_parser に伝播することを確認 (parse_user の単体テストとは独立)
    let err = parse(&["http://localhost:8080/s", "-u", "alice"]).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("user:pass"),
        "expected `user:pass` mention, got: {msg}"
    );
}

// ---------- --header ----------

#[test]
fn parses_single_header() {
    let args = parse(&["http://localhost:8080/s", "-H", "X-Foo: bar"]).unwrap();
    assert_eq!(args.headers.len(), 1);
    let (name, value) = &args.headers[0];
    assert_eq!(name.as_str(), "x-foo");
    assert_eq!(value.to_str().unwrap(), "bar");
}

#[test]
fn parses_multiple_headers() {
    // 繰り返し指定 (clap の `Vec<T>` パスを実 argv 経由で確認)
    let args = parse(&[
        "http://localhost:8080/s",
        "-H",
        "X-Foo: bar",
        "-H",
        "Authorization: Bearer xyz",
    ])
    .unwrap();
    assert_eq!(args.headers.len(), 2);
    assert_eq!(args.headers[0].0.as_str(), "x-foo");
    assert_eq!(args.headers[1].0.as_str(), "authorization");
    assert_eq!(args.headers[1].1.to_str().unwrap(), "Bearer xyz");
}

#[test]
fn header_default_is_empty() {
    let args = parse(&["http://localhost:8080/s"]).unwrap();
    assert!(args.headers.is_empty());
}

#[test]
fn rejects_header_without_colon() {
    let err = parse(&["http://localhost:8080/s", "-H", "X-Foo-Bar"]).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("K: V"), "expected `K: V` mention, got: {msg}");
}

// ---------- --interval ----------

#[test]
fn parses_interval_short_form() {
    let args = parse(&["http://localhost:8080/s", "-i", "0.5"]).unwrap();
    assert_eq!(args.interval, 0.5);
}

#[test]
fn parses_interval_long_form() {
    let args = parse(&["http://localhost:8080/s", "--interval", "0.5"]).unwrap();
    assert_eq!(args.interval, 0.5);
}

#[test]
fn interval_default_is_1_0() {
    let args = parse(&["http://localhost:8080/s"]).unwrap();
    assert_eq!(args.interval, 1.0);
}

#[test]
fn rejects_interval_out_of_range() {
    assert!(parse(&["http://localhost:8080/s", "-i", "0.0"]).is_err());
    assert!(parse(&["http://localhost:8080/s", "-i", "61.0"]).is_err());
}

// ---------- --insecure ----------

#[test]
fn parses_insecure_flag() {
    let args = parse(&["http://localhost:8080/s", "--insecure"]).unwrap();
    assert!(args.insecure);
}

#[test]
fn insecure_default_is_false() {
    let args = parse(&["http://localhost:8080/s"]).unwrap();
    assert!(!args.insecure);
}

// ---------- --no-color ----------

#[test]
fn parses_no_color_flag() {
    let args = parse(&["http://localhost:8080/s", "--no-color"]).unwrap();
    assert!(args.no_color);
}

#[test]
fn no_color_default_is_false() {
    let args = parse(&["http://localhost:8080/s"]).unwrap();
    assert!(!args.no_color);
}

// ---------- NO_COLOR 環境変数 ----------

#[test]
fn no_color_effective_when_env_set_non_empty() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _guard = EnvGuard::set("NO_COLOR", "1");

    let args = parse(&["http://localhost:8080/s"]).unwrap();
    assert!(!args.no_color, "flag は未設定");
    assert!(
        args.no_color_effective(),
        "NO_COLOR=1 だけで effective は true"
    );
}

#[test]
fn no_color_effective_false_when_env_empty_and_flag_unset() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _guard = EnvGuard::set("NO_COLOR", "");

    let args = parse(&["http://localhost:8080/s"]).unwrap();
    assert!(
        !args.no_color_effective(),
        "空文字列の NO_COLOR は未設定扱い"
    );
}

#[test]
fn no_color_effective_when_env_unset_but_flag_set() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _guard = EnvGuard::remove("NO_COLOR");

    let args = parse(&["http://localhost:8080/s", "--no-color"]).unwrap();
    assert!(args.no_color_effective(), "--no-color 単体で effective");
}

// ---------- URL は必須 ----------

#[test]
fn url_is_required_positional() {
    // URL を渡さないとパースエラーになる契約をロックする
    assert!(Args::try_parse_from(["vozltop"]).is_err());
}
