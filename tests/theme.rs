//! `Theme::from_args` の `NO_COLOR` 環境変数 × `--no-color` フラグの
//! OR 評価を end-to-end でロックする (issue #37)。
//!
//! `src/theme.rs::tests` には inline テストがあるが、`tests/theme.rs` は
//! 「`clap` パース → `Args` → `Theme::from_args` → `mono` / `color`」の
//! 公開 API パスを **テスト binary 外** から行使する。

use std::ffi::OsString;
use std::sync::Mutex;

use clap::Parser;
use vozltop::cli::Args;
use vozltop::theme::Theme;

/// `NO_COLOR` 環境変数を触るテストの直列化用ロック。
///
/// `std::env::set_var` はプロセスグローバル。`tests/theme.rs` は独立した
/// test binary なので、ここの Mutex は本ファイル内のテスト群だけを直列化
/// する (cli.rs 側の Mutex とは別プロセス)。
static ENV_LOCK: Mutex<()> = Mutex::new(());

struct EnvGuard {
    key: &'static str,
    original: Option<OsString>,
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

fn parse(args: &[&str]) -> Args {
    let mut v = vec!["vozltop"];
    v.extend_from_slice(args);
    Args::try_parse_from(v).expect("test args must parse")
}

// ---------- NO_COLOR env と --no-color の OR 評価 (4 ケースの真理値表) ----------

#[test]
fn neither_flag_nor_env_yields_color_theme() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _guard = EnvGuard::remove("NO_COLOR");

    let args = parse(&["http://localhost:8080/s"]);
    let theme = Theme::from_args(&args);
    assert!(!theme.mono, "NO_COLOR 未設定 + flag 無し → color");
}

#[test]
fn empty_env_alone_still_yields_color_theme() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _guard = EnvGuard::set("NO_COLOR", "");

    let args = parse(&["http://localhost:8080/s"]);
    let theme = Theme::from_args(&args);
    assert!(
        !theme.mono,
        "no-color.org 準拠で空文字列 NO_COLOR は「未設定」扱い → color"
    );
}

#[test]
fn flag_only_yields_mono_theme() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _guard = EnvGuard::remove("NO_COLOR");

    let args = parse(&["http://localhost:8080/s", "--no-color"]);
    let theme = Theme::from_args(&args);
    assert!(theme.mono, "--no-color のみで mono");
}

#[test]
fn env_only_yields_mono_theme() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _guard = EnvGuard::set("NO_COLOR", "1");

    let args = parse(&["http://localhost:8080/s"]);
    let theme = Theme::from_args(&args);
    assert!(theme.mono, "NO_COLOR=1 のみで mono");
}

#[test]
fn flag_and_env_both_yield_mono_theme() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _guard = EnvGuard::set("NO_COLOR", "1");

    let args = parse(&["http://localhost:8080/s", "--no-color"]);
    let theme = Theme::from_args(&args);
    assert!(theme.mono, "flag + env 両方で mono");
}

// ---------- mono / color の挙動の最低限の確認 ----------

#[test]
fn mono_theme_has_error_banner_prefix() {
    // `[!]` プレフィックスが効くことを公開 API で確認 (issue #26 受け入れ条件)
    assert_eq!(Theme::from_no_color(true).error_banner_prefix(), "[!] ");
    assert_eq!(Theme::from_no_color(false).error_banner_prefix(), "");
}
