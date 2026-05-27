//! バイナリエントリ。TUI を起動して `tokio::select!` ループを回す (issue #25)。
//!
//! 起動シーケンス:
//!
//! 1. `color_eyre::install()` — エラーレポータ。panic hook を先に仕込むため
//!    `terminal::install_panic_hook()` より前に呼ぶ (issue #42)。
//! 2. `Args::parse()` — CLI 引数を確定。
//! 3. `enable_raw_mode` + `EnterAlternateScreen` + `Hide` — terminal を TUI 用に
//!    切り替える。
//! 4. `terminal::install_panic_hook()` — color_eyre が仕込んだ hook を previous
//!    として捕まえつつ、その手前で `terminal::restore()` を差し込む。これにより
//!    `enable_raw_mode` 以降の **任意の panic** で必ず端末が復旧する。
//! 5. `event_loop()` を回す。
//! 6. 正常終了経路でも `terminal::restore()` を必ず呼ぶ。
//!
//! ループのイベント分配:
//!
//! - `tokio::time::interval(args.interval)`: 一定間隔で fetch task を spawn
//!   (single-flight: 前 fetch が未完了なら skip)。
//! - `mpsc::Receiver<Result<VtsStatus, FetchError>>`: fetch task の完了通知。
//!   受信したら `App::on_fetch_ok` / `App::on_fetch_err` に渡す。
//! - `crossterm::event::EventStream`: terminal 入力。`event::map_event` で
//!   `AppEvent::Quit` / `Key` / `Resize` に正規化し、`Quit` でループを抜ける。
//! - `tokio::signal::ctrl_c()`: TUI 外 (e.g. プロセスを kill -INT) からの SIGINT。
//! - `tokio::signal::unix::SignalKind::terminate()` (unix のみ): SIGTERM。
//!
//! 描画は毎ループ末尾で `terminal.draw()` を 1 回呼ぶ。tick / fetch result /
//! Key / Resize のどれが来てもとりあえず再描画する (1Hz 程度なら過剰では無い)。

use std::io::{self, Stdout};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use color_eyre::eyre::{Result, WrapErr};
use crossterm::cursor::Hide;
use crossterm::event::{EventStream, KeyCode};
use crossterm::execute;
use crossterm::terminal::{enable_raw_mode, EnterAlternateScreen};
use futures_util::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{interval, MissedTickBehavior};

use vozltop::cli::Args;
use vozltop::client::{FetchError, VtsClient};
use vozltop::event::{map_event, AppEvent};
use vozltop::model::VtsStatus;
use vozltop::state::App;
use vozltop::terminal as term;
use vozltop::theme::Theme;
use vozltop::ui;

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    if let Err(err) = run().await {
        // run() 内で raw mode 中に Err が返ってきても、run() 末尾で restore は
        // 走っている前提。それでも保険として再度 restore する (冪等)。
        term::restore();
        eprintln!("vozltop: {err:?}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

async fn run() -> Result<()> {
    color_eyre::install()?;
    let args = Args::parse();

    // CLI / HTTP セットアップは TUI 起動前に済ませる。ここで失敗した場合は
    // raw mode に入っていないので restore 不要。
    let theme = Theme::from_args(&args);
    let client = Arc::new(VtsClient::new(&args)?);
    let interval_secs = args.interval;

    // ここから先で何が起きても必ず restore を通すため、setup の各段は最小化。
    setup_terminal().wrap_err("failed to enter TUI mode")?;
    // panic hook は setup_terminal 後・event_loop 前に install する。
    // color_eyre::install() の hook が「previous」として保存される。
    term::install_panic_hook();

    let mut terminal =
        Terminal::new(CrosstermBackend::new(io::stdout())).wrap_err("failed to init ratatui")?;
    let mut app = App::with_theme(theme);

    let loop_result = event_loop(&mut terminal, &mut app, client, interval_secs).await;

    // 正常 / 異常どちらの経路でも raw mode を抜く。restore は冪等 (idempotent)。
    term::restore();
    loop_result
}

fn setup_terminal() -> Result<()> {
    enable_raw_mode().wrap_err("enable_raw_mode failed")?;
    execute!(io::stdout(), EnterAlternateScreen, Hide)
        .wrap_err("EnterAlternateScreen / Hide cursor failed")?;
    Ok(())
}

/// メインの `tokio::select!` ループ。
///
/// 終了条件:
///
/// - terminal イベントが `AppEvent::Quit` (q / Q / Ctrl+C / F10)
/// - `tokio::signal::ctrl_c()` (SIGINT)
/// - SIGTERM (unix のみ)
/// - `EventStream` が `None` を返した (stdin EOF; 想定外だが防御的に exit)
async fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut App,
    client: Arc<VtsClient>,
    interval_secs: f64,
) -> Result<()> {
    let mut ticker = interval(Duration::from_secs_f64(interval_secs));
    // `Burst` (default) だと tick が溜まったあと連続発火するので、TUI 用に
    // `Skip` で「直近 1 回だけ」に絞る (古い tick は捨てる)。
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    // interval は最初の tick が即時返るので 1 度空読みする (本体ループの 1
    // 反復目で fetch を即発火させたいので、ticker.tick() を 1 度通しておく)。
    ticker.tick().await;

    // fetch 結果は mpsc(1) で受け取る。channel cap=1 で背圧をかけ、
    // 1 fetch ぶんしか溜めないことで RAM を抑える。
    let (fetch_tx, mut fetch_rx) = mpsc::channel::<Result<VtsStatus, FetchError>>(1);
    let mut in_flight: Option<JoinHandle<()>> = None;

    let mut events = EventStream::new();

    // SIGTERM (unix) の Future を 1 つ用意して pin する。fire したら break に使う。
    // non-unix では Pending を返してずっと resolve しない。
    let sigterm_fut = sigterm_future();
    tokio::pin!(sigterm_fut);

    // 起動直後の 1 描画 (Connecting バナー)。
    terminal
        .draw(|f| ui::render(f, app))
        .wrap_err("initial draw failed")?;

    // 起動直後の 1 fetch も即時に走らせる (interval の 1 周目を待たない)。
    spawn_fetch(&client, &fetch_tx, &mut in_flight);

    loop {
        tokio::select! {
            // 定期的な fetch trigger。前 fetch が未完了なら skip し、tick だけは
            // 受け流す (= drain しないと次回 tick がブロックされる)。
            _ = ticker.tick() => {
                spawn_fetch(&client, &fetch_tx, &mut in_flight);
                // tick 自体では app 状態は変わらないが、保守的に再描画する。
            }

            // fetch task の完了通知。
            Some(result) = fetch_rx.recv() => {
                match result {
                    Ok(status) => app.on_fetch_ok(status),
                    Err(err) => app.on_fetch_err(&err),
                }
            }

            // terminal 入力。
            maybe_ev = events.next() => {
                match maybe_ev {
                    Some(Ok(crossterm_ev)) => {
                        if let Some(app_ev) = map_event(crossterm_ev) {
                            match app_ev {
                                AppEvent::Quit => break,
                                AppEvent::Key(k) => {
                                    // issue #28: カーソル移動。ソート / フィルタは #31。
                                    // CLAUDE.md のキー割り当てに従う:
                                    //   ↑ / k → 1 行上
                                    //   ↓ / j → 1 行下
                                    //   PgUp / PgDn → 1 ページぶん移動
                                    match k.code {
                                        KeyCode::Up | KeyCode::Char('k') | KeyCode::Char('K') => {
                                            app.cursor_up();
                                        }
                                        KeyCode::Down | KeyCode::Char('j') | KeyCode::Char('J') => {
                                            app.cursor_down();
                                        }
                                        KeyCode::PageUp => app.cursor_page_up(),
                                        KeyCode::PageDown => app.cursor_page_down(),
                                        _ => {}
                                    }
                                }
                                AppEvent::Resize(_, _) => {
                                    // resize 自体は再描画だけで吸収する (TableState が
                                    // 自動でスクロールを再計算)。
                                }
                                AppEvent::Tick(_) | AppEvent::FetchErr(_) => {
                                    // crossterm の Event 由来からは出ない variant。
                                    debug_assert!(false, "map_event は Tick/FetchErr を作らない");
                                }
                            }
                        }
                    }
                    Some(Err(err)) => {
                        // EventStream のエラー (まれだが TTY が消えた場合等) は
                        // ループ終了で扱う。
                        return Err(color_eyre::eyre::eyre!("terminal event stream error: {err}"));
                    }
                    None => break, // stdin EOF: 想定外だが safe-exit
                }
            }

            // SIGINT (ctrl_c) / SIGTERM (unix)
            _ = tokio::signal::ctrl_c() => break,
            _ = &mut sigterm_fut => break,
        }

        terminal
            .draw(|f| ui::render(f, app))
            .wrap_err("draw failed")?;
    }

    Ok(())
}

/// `client.fetch()` を spawn し、結果を `tx` 経由で push する task を作る。
///
/// **single-flight**: `in_flight` がまだ完了していなければ no-op。これにより
/// 遅い fetch + 短い interval 設定でも task が溜まらない。
fn spawn_fetch(
    client: &Arc<VtsClient>,
    tx: &mpsc::Sender<Result<VtsStatus, FetchError>>,
    in_flight: &mut Option<JoinHandle<()>>,
) {
    if in_flight.as_ref().is_some_and(|h| !h.is_finished()) {
        // 前回 fetch がまだ走っているので、今回の tick は捨てる。
        // 例: interval=0.5s で fetch が 1.2s かかる構成。
        return;
    }
    let client = Arc::clone(client);
    let tx = tx.clone();
    *in_flight = Some(tokio::spawn(async move {
        let result = client.fetch().await;
        // recv 側が drop 済み (= ループが exit 中) なら send は失敗する。無視。
        let _ = tx.send(result).await;
    }));
}

/// SIGTERM を 1 度受けたら resolve する Future。
///
/// unix: `tokio::signal::unix::Signal::recv` を 1 度 await する。
/// non-unix: `std::future::pending()` を await してずっと resolve しない (no-op)。
///
/// 戻り値の Future を `tokio::pin!` してから select! の `&mut sigterm_fut`
/// 枝で待つ。
async fn sigterm_future() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => {
                // 登録に失敗した環境ではフォールバックして Pending (no-op)。
                std::future::pending::<()>().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        std::future::pending::<()>().await;
    }
}
