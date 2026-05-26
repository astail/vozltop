//! 最小バイナリエントリ。
//!
//! `cargo run -- <URL> [OPTIONS]` で `VtsClient` を 1 回回し、結果を `App` に
//! 渡してからバナーと `VtsStatus` を `Debug` 出力する。issue #25 で
//! tokio::select! ループ + TUI 起動に置き換わる。
//!
//! issue #34 で `clap::Parser` に置き換え、`--interval` / `--no-color` を
//! 含む全フラグを受け取れるようになった。

use std::process::ExitCode;

use clap::Parser;
use color_eyre::eyre::Result;
use vozltop::cli::Args;
use vozltop::client::VtsClient;
use vozltop::state::App;

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    if let Err(err) = run().await {
        eprintln!("vozltop: {err:?}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

async fn run() -> Result<()> {
    color_eyre::install()?;
    let args = Args::parse();

    let client = VtsClient::new(&args)?;
    let mut app = App::new();

    match client.fetch().await {
        Ok(status) => app.on_fetch_ok(status),
        Err(err) => app.on_fetch_err(&err),
    }

    println!("[status] {:?}", app.status);
    if let Some(banner) = &app.error_banner {
        println!("[banner] {banner}");
    }
    if app.history.nginx_restart_detected() {
        println!("[banner] nginx restart detected (nowMsec went backwards)");
    }
    if let Some(snapshot) = app.history.latest() {
        println!("{:#?}", snapshot.status);
    }
    Ok(())
}
