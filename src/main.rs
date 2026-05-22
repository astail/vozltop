//! issue #17 で wiring 開始、issue #19 で App / FetchError 経路に置き換えた
//! 最小バイナリエントリ。
//!
//! `cargo run -- <URL>` で `VtsClient` を 1 回回し、結果を `App` に渡してから
//! バナーと `VtsStatus` を `Debug` 出力する。issue #25 で tokio::select! ループ
//! + TUI 起動に置き換わる。

use std::env;
use std::process::ExitCode;

use color_eyre::eyre::{eyre, Result, WrapErr};
use url::Url;
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

    let url_arg = env::args().nth(1).ok_or_else(|| {
        eyre!("usage: vozltop <URL>  (例: http://127.0.0.1:8080/status/format/json)")
    })?;
    let url: Url = url_arg
        .parse()
        .wrap_err_with(|| format!("invalid URL: {url_arg}"))?;

    // --user / --header / --insecure はまだ clap が無いため CLI から読まない。
    // issue #34 で clap derive を入れたら env::args()/clap::parse() に置換する。
    let args = Args {
        url,
        user: None,
        headers: Vec::new(),
        insecure: false,
    };
    let client = VtsClient::new(&args)?;
    let mut app = App::new();

    match client.fetch().await {
        Ok(status) => app.on_fetch_ok(status),
        Err(err) => app.on_fetch_err(&err),
    }

    println!("[banner] {}", app.banner.label());
    if app.restart_detected {
        println!("[banner] nginx restart detected (nowMsec went backwards)");
    }
    if let Some(latest) = &app.latest {
        println!("{latest:#?}");
    }
    Ok(())
}
