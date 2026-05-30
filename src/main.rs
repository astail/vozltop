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
use crossterm::event::{EventStream, KeyCode, KeyEvent};
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
use vozltop::state::{AlertConfig, App};
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
    app.alerts = AlertConfig::from_args(&args);

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
                    Ok(status) => {
                        app.on_fetch_ok(status);
                        // アラート行が新たに出現したら端末ベルを 1 度鳴らす (issue #47)。
                        if app.update_alert_active(ui::table::any_row_alerting(app)) {
                            ring_bell();
                        }
                    }
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
                                AppEvent::Key(k) => handle_key(app, k),
                                AppEvent::Resize(_, _) => {
                                    // resize 自体は再描画だけで吸収する。ratatui の
                                    // `terminal.draw` が autoresize し、`TableState` が
                                    // スクロール位置を再計算する。
                                }
                                AppEvent::Tick(_) | AppEvent::FetchErr(_) => {
                                    // `map_event` の契約上、crossterm Event 由来では
                                    // 生成されない variant。リリースビルドでも気付けるよう
                                    // `unreachable!` で明示する。
                                    unreachable!("map_event は Tick/FetchErr を作らない");
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

/// `AppEvent::Key` を `App` に反映する純関数。
///
/// 扱うキー: F1 / `?` (help)、Enter (詳細オーバーレイを開く #32)、
/// Esc (filter → help → detail の順で閉じる)、カーソル移動 (#28)、
/// ソート (1-9 / F5) + フィルタ (F4 / `/` + 文字入力 / Backspace) (#31)、
/// Tab / Shift+Tab で zone 種別タブ切替 (#104)。
///
/// テスト容易性のため `App` への &mut 操作だけを引数に取り、terminal/IO は触らない。
fn handle_key(app: &mut App, key: KeyEvent) {
    // フィルタ入力モード中は、ほとんどのキーを「フィルタ文字列の編集」として扱う。
    // ただし矢印キー等のカーソル移動は素通しして、絞り込みながら行を選べるように
    // する (issue #31 受け入れ条件「フィルタ中も矢印キーカーソルが効く」)。
    if app.filter_active {
        match key.code {
            // Esc / Enter で入力モードを抜ける。Esc は filter を空に戻す
            // (最優先。下の通常 Esc 分岐より先に処理する)。Enter は filter を
            // 残したまま確定して通常モードへ。
            KeyCode::Esc => app.clear_filter(),
            KeyCode::Enter => app.filter_active = false,
            KeyCode::Backspace => app.pop_filter_char(),
            // カーソル移動はフィルタ中も有効。
            KeyCode::Up => app.cursor_up(),
            KeyCode::Down => app.cursor_down(),
            KeyCode::PageUp => app.cursor_page_up(),
            KeyCode::PageDown => app.cursor_page_down(),
            // 通常の文字は filter に追加する。制御文字は無視。
            KeyCode::Char(c) if !c.is_control() => app.push_filter_char(c),
            _ => {}
        }
        return;
    }

    match key.code {
        // F1 / `?` で help モーダルを toggle (#33 受け入れ条件)。`?` は letter alias。
        KeyCode::F(1) | KeyCode::Char('?') => {
            app.show_help = !app.show_help;
        }
        // F4 / `/` でフィルタ入力モードに入る (#31)。help / detail が開いている
        // ときは無視する (モーダル優先)。
        KeyCode::F(4) | KeyCode::Char('/') if !app.show_help && app.detail_zone.is_none() => {
            app.enter_filter();
        }
        // F5: ソート方向反転 (#31)。help / detail が開いているときは無視する
        // (モーダル優先。filter 入力と同じガード条件)。
        KeyCode::F(5) if !app.show_help && app.detail_zone.is_none() => {
            app.toggle_sort_dir();
        }
        // 数字キー 1-9: ソート列指定 (#31)。同じ列なら方向反転。help / detail が
        // 開いているときは無視する (モーダル優先)。
        KeyCode::Char(c @ '1'..='9') if !app.show_help && app.detail_zone.is_none() => {
            // '1'..='9' なので to_digit は必ず Some。
            if let Some(d) = c.to_digit(10) {
                app.apply_sort_key(d as u8);
            }
        }
        // Enter: cursor が指す zone の詳細オーバーレイを開く (#32)。
        // help が開いているときは Enter を無視する (モーダル優先)。
        KeyCode::Enter if !app.show_help => {
            if let Some(zone) = ui::table::selected_zone(app) {
                app.detail_zone = Some(zone);
            }
        }
        // Esc は help → detail の順に閉じる (filter 入力中は上の早期 return で処理済み)。
        KeyCode::Esc if app.show_help => {
            app.show_help = false;
        }
        KeyCode::Esc if app.detail_zone.is_some() => {
            app.detail_zone = None;
        }
        // Esc: filter が非空 (入力モードは抜けたが絞り込みは残っている) なら解除。
        KeyCode::Esc if !app.filter.is_empty() => {
            app.clear_filter();
        }
        // issue #28: 行カーソル移動 (CLAUDE.md キー割り当て準拠)。
        KeyCode::Up | KeyCode::Char('k') | KeyCode::Char('K') => {
            app.cursor_up();
        }
        KeyCode::Down | KeyCode::Char('j') | KeyCode::Char('J') => {
            app.cursor_down();
        }
        KeyCode::PageUp => app.cursor_page_up(),
        KeyCode::PageDown => app.cursor_page_down(),
        // issue #104: Tab / Shift+Tab で zone 種別タブを循環する。
        // help / detail オーバーレイ表示中は他のソート / フィルタ操作と同じく
        // モーダル優先で無効化する (filter 入力モードは上の早期 return で処理済み)。
        KeyCode::Tab if !app.show_help && app.detail_zone.is_none() => {
            app.next_tab();
        }
        KeyCode::BackTab if !app.show_help && app.detail_zone.is_none() => {
            app.prev_tab();
        }
        _ => {}
    }
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

/// 端末ベル (BEL = `\x07`) を 1 度鳴らす (issue #47)。
///
/// BEL は制御文字なので alternate screen のバッファ位置を動かさず、ratatui の
/// 描画と干渉しない。出力失敗 (端末が消えた等) は致命的でないので無視する。
fn ring_bell() {
    use std::io::Write;
    let mut out = io::stdout();
    let _ = out.write_all(b"\x07");
    let _ = out.flush();
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

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEventKind, KeyEventState, KeyModifiers};
    use vozltop::model::VtsStatus;

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    /// serverZones 入りの最小 snapshot を push した App を作る。
    fn app_with_server_zone(zone: &str) -> App {
        let raw = serde_json::json!({
            "hostName": "h", "nginxVersion": "1", "moduleVersion": "v",
            "loadMsec": 0u64, "nowMsec": 1000u64,
            "connections": {
                "active": 0, "reading": 0, "writing": 0,
                "waiting": 0, "accepted": 0, "handled": 0, "requests": 0
            },
            "serverZones": {
                zone: {
                    "requestCounter": 0, "inBytes": 0, "outBytes": 0,
                    "responses": {
                        "1xx": 0, "2xx": 0, "3xx": 0, "4xx": 0, "5xx": 0,
                        "miss": 0, "bypass": 0, "expired": 0, "stale": 0,
                        "updating": 0, "revalidated": 0, "hit": 0, "scarce": 0
                    },
                    "requestMsec": 0, "requestMsecCounter": 0,
                    "requestBuckets": { "msecs": [], "counters": [] }
                }
            }
        });
        let status: VtsStatus = serde_json::from_value(raw).unwrap();
        let mut app = App::new();
        app.on_fetch_ok(status);
        app
    }

    #[test]
    fn f1_toggles_help_overlay() {
        let mut app = App::new();
        assert!(!app.show_help);
        handle_key(&mut app, press(KeyCode::F(1)));
        assert!(app.show_help, "F1 should open help");
        handle_key(&mut app, press(KeyCode::F(1)));
        assert!(!app.show_help, "F1 again should close help");
    }

    #[test]
    fn question_mark_is_letter_alias_for_f1() {
        // macOS Terminal.app が F1 を奪うため、? を letter alias として受け付ける
        // (CLAUDE.md / issue #33 受け入れ条件)。
        let mut app = App::new();
        handle_key(&mut app, press(KeyCode::Char('?')));
        assert!(app.show_help, "? should open help");
        handle_key(&mut app, press(KeyCode::Char('?')));
        assert!(!app.show_help, "? again should close help");
    }

    #[test]
    fn esc_closes_help_when_open() {
        let mut app = App::new();
        app.show_help = true;
        handle_key(&mut app, press(KeyCode::Esc));
        assert!(!app.show_help, "Esc should close help");
    }

    #[test]
    fn esc_is_noop_when_help_is_already_closed() {
        // 後続 issue (#31 / #32) で Esc は filter / detail を閉じるためにも使う。
        // 本 PR では help が closed のとき Esc は no-op (= 他状態に副作用無し)。
        let mut app = App::new();
        app.show_help = false;
        // Esc を打っても show_help は false のまま、他フィールドにも触らない。
        handle_key(&mut app, press(KeyCode::Esc));
        assert!(!app.show_help);
    }

    #[test]
    fn unrelated_keys_do_not_change_help_state() {
        let mut app = App::new();
        handle_key(&mut app, press(KeyCode::Tab));
        handle_key(&mut app, press(KeyCode::Char('x')));
        handle_key(&mut app, press(KeyCode::F(5)));
        assert!(!app.show_help);
    }

    // ---------- detail overlay (issue #32) ----------

    #[test]
    fn enter_opens_detail_for_selected_zone() {
        let mut app = app_with_server_zone("alpha");
        assert!(app.detail_zone.is_none());
        handle_key(&mut app, press(KeyCode::Enter));
        assert_eq!(app.detail_zone.as_deref(), Some("alpha"));
    }

    #[test]
    fn enter_is_noop_without_snapshot() {
        // snapshot 未取得 (Connecting) では選択 zone が無いので Enter は no-op。
        let mut app = App::new();
        handle_key(&mut app, press(KeyCode::Enter));
        assert!(app.detail_zone.is_none());
    }

    #[test]
    fn enter_ignored_while_help_open() {
        // help モーダルが開いているときは Enter で detail を開かない。
        let mut app = app_with_server_zone("alpha");
        app.show_help = true;
        handle_key(&mut app, press(KeyCode::Enter));
        assert!(app.detail_zone.is_none());
    }

    #[test]
    fn esc_closes_detail_when_open() {
        let mut app = app_with_server_zone("alpha");
        handle_key(&mut app, press(KeyCode::Enter));
        assert!(app.detail_zone.is_some());
        handle_key(&mut app, press(KeyCode::Esc));
        assert!(app.detail_zone.is_none(), "Esc should close detail");
    }

    // ---------- ソート / フィルタ (issue #31) ----------

    fn press_char(c: char) -> KeyEvent {
        press(KeyCode::Char(c))
    }

    #[test]
    fn digit_key_sets_sort_column() {
        let mut app = App::new();
        // default は col 1 (RPS) なので別列 key 3 = 2xx% (col index 2) で選択を見る。
        handle_key(&mut app, press_char('3'));
        assert_eq!(app.sort.column, 2);
        assert!(app.sort.descending);
    }

    #[test]
    fn same_digit_key_toggles_direction() {
        let mut app = App::new();
        // default (col 1) と別の列を選んでから同キー連打で toggle を見る。
        handle_key(&mut app, press_char('3'));
        assert!(app.sort.descending);
        handle_key(&mut app, press_char('3'));
        assert!(!app.sort.descending, "second press toggles to ascending");
    }

    #[test]
    fn f5_toggles_sort_direction() {
        let mut app = App::new();
        assert!(app.sort.descending);
        handle_key(&mut app, press(KeyCode::F(5)));
        assert!(!app.sort.descending);
        handle_key(&mut app, press(KeyCode::F(5)));
        assert!(app.sort.descending);
    }

    #[test]
    fn f4_enters_filter_mode() {
        let mut app = App::new();
        handle_key(&mut app, press(KeyCode::F(4)));
        assert!(app.filter_active);
    }

    #[test]
    fn slash_enters_filter_mode() {
        let mut app = App::new();
        handle_key(&mut app, press_char('/'));
        assert!(app.filter_active);
    }

    #[test]
    fn typing_in_filter_mode_appends_chars() {
        let mut app = App::new();
        handle_key(&mut app, press(KeyCode::F(4)));
        handle_key(&mut app, press_char('a'));
        handle_key(&mut app, press_char('p'));
        handle_key(&mut app, press_char('i'));
        assert_eq!(app.filter, "api");
    }

    #[test]
    fn backspace_in_filter_mode_removes_last_char() {
        let mut app = App::new();
        app.enter_filter();
        for c in ['a', 'b', 'c'] {
            handle_key(&mut app, press_char(c));
        }
        handle_key(&mut app, press(KeyCode::Backspace));
        assert_eq!(app.filter, "ab");
    }

    #[test]
    fn esc_in_filter_mode_clears_filter() {
        let mut app = App::new();
        app.enter_filter();
        handle_key(&mut app, press_char('x'));
        handle_key(&mut app, press(KeyCode::Esc));
        assert!(!app.filter_active);
        assert!(app.filter.is_empty());
    }

    #[test]
    fn enter_in_filter_mode_confirms_keeps_filter() {
        let mut app = App::new();
        app.enter_filter();
        handle_key(&mut app, press_char('a'));
        handle_key(&mut app, press(KeyCode::Enter));
        assert!(!app.filter_active, "Enter exits input mode");
        assert_eq!(app.filter, "a", "Enter keeps the filter applied");
    }

    #[test]
    fn cursor_works_during_filter_mode() {
        // 受け入れ条件: フィルタ中も矢印キーカーソルが効く。
        let mut app = app_with_server_zone("alpha");
        app.visible_rows.set(5);
        app.enter_filter();
        handle_key(&mut app, press(KeyCode::Down));
        assert_eq!(app.cursor, 1, "Down moves cursor while filtering");
        handle_key(&mut app, press(KeyCode::Up));
        assert_eq!(app.cursor, 0, "Up moves cursor while filtering");
    }

    #[test]
    fn esc_clears_applied_filter_after_confirm() {
        // 入力モードを Enter で抜けたあと、Esc で残った filter を解除できる。
        let mut app = App::new();
        app.enter_filter();
        handle_key(&mut app, press_char('a'));
        handle_key(&mut app, press(KeyCode::Enter));
        assert_eq!(app.filter, "a");
        handle_key(&mut app, press(KeyCode::Esc));
        assert!(app.filter.is_empty(), "Esc clears the leftover filter");
    }

    #[test]
    fn digit_keys_ignored_while_help_open() {
        let mut app = App::new();
        app.show_help = true;
        handle_key(&mut app, press_char('3'));
        assert_eq!(
            app.sort.column, 1,
            "sort unchanged (default RPS) while help open"
        );
    }

    #[test]
    fn f4_ignored_while_help_open() {
        let mut app = App::new();
        app.show_help = true;
        handle_key(&mut app, press(KeyCode::F(4)));
        assert!(!app.filter_active, "filter not entered while help open");
    }

    #[test]
    fn digit_keys_ignored_while_detail_open() {
        // detail オーバーレイ表示中は裏のテーブルを並び替えない (filter と同じガード)。
        let mut app = App::new();
        app.detail_zone = Some("alpha".to_string());
        handle_key(&mut app, press_char('3'));
        assert_eq!(
            app.sort.column, 1,
            "sort unchanged (default RPS) while detail open"
        );
    }

    #[test]
    fn f5_ignored_while_detail_open() {
        let mut app = App::new();
        app.detail_zone = Some("alpha".to_string());
        assert!(app.sort.descending);
        handle_key(&mut app, press(KeyCode::F(5)));
        assert!(
            app.sort.descending,
            "sort direction unchanged while detail open"
        );
    }

    #[test]
    fn esc_closes_help_before_detail() {
        // help と detail が両方開いているとき、Esc はまず help を閉じる。
        let mut app = app_with_server_zone("alpha");
        handle_key(&mut app, press(KeyCode::Enter));
        app.show_help = true;
        handle_key(&mut app, press(KeyCode::Esc));
        assert!(!app.show_help, "first Esc closes help");
        assert!(
            app.detail_zone.is_some(),
            "detail stays open after first Esc"
        );
        handle_key(&mut app, press(KeyCode::Esc));
        assert!(app.detail_zone.is_none(), "second Esc closes detail");
    }

    // ---------- タブ切替 (issue #104) ----------

    use vozltop::state::Tab;

    #[test]
    fn tab_key_cycles_through_zone_tabs() {
        let mut app = App::new();
        assert_eq!(app.active_tab, Tab::Server);
        handle_key(&mut app, press(KeyCode::Tab));
        assert_eq!(app.active_tab, Tab::Upstream);
        handle_key(&mut app, press(KeyCode::Tab));
        assert_eq!(app.active_tab, Tab::Cache);
        handle_key(&mut app, press(KeyCode::Tab));
        assert_eq!(app.active_tab, Tab::Filter);
        handle_key(&mut app, press(KeyCode::Tab));
        assert_eq!(app.active_tab, Tab::Server, "Filter の次は Server");
    }

    #[test]
    fn shift_tab_cycles_backwards() {
        // crossterm は Shift+Tab を `KeyCode::BackTab` として配信する。
        let mut app = App::new();
        handle_key(&mut app, press(KeyCode::BackTab));
        assert_eq!(app.active_tab, Tab::Filter, "Server の前は Filter");
        handle_key(&mut app, press(KeyCode::BackTab));
        assert_eq!(app.active_tab, Tab::Cache);
    }

    #[test]
    fn tab_ignored_while_help_open() {
        let mut app = App::new();
        app.show_help = true;
        handle_key(&mut app, press(KeyCode::Tab));
        assert_eq!(
            app.active_tab,
            Tab::Server,
            "help モーダル中は Tab で切り替えない"
        );
    }

    #[test]
    fn tab_ignored_while_detail_open() {
        let mut app = App::new();
        app.detail_zone = Some("alpha".to_string());
        handle_key(&mut app, press(KeyCode::Tab));
        assert_eq!(
            app.active_tab,
            Tab::Server,
            "detail オーバーレイ中は Tab で切り替えない"
        );
    }

    #[test]
    fn tab_ignored_while_filter_input_active() {
        // F4 / `/` でフィルタ入力モードに入った後は、Tab を文字入力扱いしない
        // (filter モード中の早期 return 内に Tab の枝が無いため、`Char(...)` 系
        // と異なり何も起きない = active_tab も filter 文字列も不変)。
        let mut app = App::new();
        app.enter_filter();
        let before_filter = app.filter.clone();
        handle_key(&mut app, press(KeyCode::Tab));
        assert_eq!(app.active_tab, Tab::Server);
        assert_eq!(
            app.filter, before_filter,
            "filter 入力モード中 Tab は no-op"
        );
    }
}
