//! ビルド済みバイナリを子プロセスで起動するスモークテスト (issue #129)。
//!
//! `tests/cli.rs` は `Args::try_parse_from` を直接呼ぶ in-process テストで、
//! `--version` / `--help` 経由の `clap::Error` (DisplayVersion / DisplayHelp)
//! が main.rs の color-eyre 流路を通って exit 1 になるリグレッションは
//! 検出できない。本ファイルは `env!("CARGO_BIN_EXE_vozltop")` で
//! cargo がビルドした実バイナリを `std::process::Command` で起動し、
//! 終了コードと標準出力を確認する。

use std::process::Command;

/// cargo がビルドしたバイナリ path を返す。
///
/// `CARGO_BIN_EXE_<name>` は integration test に対して cargo が自動でセットする
/// 環境変数で、`target/<profile>/<name>(.exe)` を指す。
fn vozltop_bin() -> &'static str {
    env!("CARGO_BIN_EXE_vozltop")
}

#[test]
fn version_flag_exits_zero_with_version_on_stdout() {
    let output = Command::new(vozltop_bin())
        .arg("--version")
        .output()
        .expect("vozltop --version should spawn");

    assert!(
        output.status.success(),
        "--version は exit 0 で終了するはず (clap の DisplayVersion 流路)。\nstatus={:?}\nstderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let expected_version = env!("CARGO_PKG_VERSION");
    assert!(
        stdout.contains(expected_version),
        "--version の stdout には Cargo.toml の version ({expected_version}) が含まれるはず。\n実際の stdout: {stdout:?}",
    );
}

#[test]
fn help_flag_exits_zero_with_usage_on_stdout() {
    let output = Command::new(vozltop_bin())
        .arg("--help")
        .output()
        .expect("vozltop --help should spawn");

    assert!(
        output.status.success(),
        "--help は exit 0 で終了するはず (clap の DisplayHelp 流路)。\nstatus={:?}\nstderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Usage:"),
        "--help の stdout には clap の Usage 行が含まれるはず。\n実際の stdout: {stdout:?}",
    );
}

#[test]
fn missing_required_url_exits_nonzero() {
    // 正常系の対照として、引数なし起動は今まで通り失敗することも確認する。
    // (DisplayHelp / DisplayVersion 以外の clap::Error を `e.exit()` に流して
    // しまっていないかのリグレッション検出。)
    let output = Command::new(vozltop_bin())
        .output()
        .expect("vozltop (no args) should spawn");

    assert!(
        !output.status.success(),
        "URL 未指定時は失敗するはず。実際は exit {:?}",
        output.status.code(),
    );
}
