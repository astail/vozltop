//! TUI のターミナル init/restore と、パニック時の自動復旧 (panic hook)。
//!
//! ratatui の raw mode + alternate screen 中に panic させると、shell に
//! 戻ったあと「入力が見えない」「改行されない」「カーソルが出ない」状態に
//! 落ちる。ssh 越しに本ツールを使う運用では端末破壊は復旧コストが大きい
//! (issue #42) ため、`std::panic::set_hook` で必ず復旧してから default
//! hook (もしくは `color_eyre::install` が仕込んだ hook) に流す。
//!
//! 呼び出し順序の前提:
//!
//! 1. `color_eyre::install()`  — color_eyre 側で reporting hook が入る
//! 2. `terminal::install_panic_hook()` — color_eyre hook を「previous」として
//!    捕まえ、その前に [`restore`] を差し込む形で wrap する
//!
//! 順番を逆にすると color_eyre 側が後から `set_hook` で上書きされ、ターミナル
//! 復旧が走らなくなる。

use std::io::{self, Write};

use crossterm::cursor::Show;
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, LeaveAlternateScreen};

/// raw mode / alternate screen / hidden cursor を解除する。
///
/// 通常終了経路でも panic hook 内でも呼べるよう、個別エラーは握りつぶす
/// (panic 中に `?` で `Result` を返しても呼び出し側で扱えないため)。何も
/// entry していない状態で呼んでも no-op に近い (各 crossterm 関数が NOP な
/// 状態に対して安全に return する) ので二度呼びにも耐える。
pub fn restore() {
    let _ = disable_raw_mode();
    let _ = execute!(io::stdout(), LeaveAlternateScreen, Show);
    let _ = io::stdout().flush();
}

/// 既存の panic hook を「previous」として保存し、その前に [`restore`] を
/// 差し込む形で `std::panic::set_hook` を上書きする。
///
/// `color_eyre::install()` の後に 1 度だけ呼ぶこと。複数回呼ぶと、wrapper
/// の中にさらに wrapper が入って restore が多重に走る (`restore` 自体は
/// 冪等なので動作は壊れないが意味は無い)。
pub fn install_panic_hook() {
    install_panic_hook_with(restore);
}

fn install_panic_hook_with<F>(restore_fn: F)
where
    F: Fn() + Send + Sync + 'static,
{
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_fn();
        prev(info);
    }));
}

#[cfg(test)]
mod tests {
    use std::panic::{self, AssertUnwindSafe};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    use super::install_panic_hook_with;

    // panic hook はプロセスグローバルなので、複数の panic-hook テストが並列
    // に走ると take_hook/set_hook が干渉する。本ファイル内のテストは必ず
    // この Mutex を取って直列化する。
    static HOOK_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn installed_hook_runs_restore_then_previous_hook() {
        let _g = HOOK_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        static RESTORE_CALLS: AtomicUsize = AtomicUsize::new(0);
        static PREV_CALLS: AtomicUsize = AtomicUsize::new(0);
        RESTORE_CALLS.store(0, Ordering::SeqCst);
        PREV_CALLS.store(0, Ordering::SeqCst);

        // 既存 hook (= cargo test ハーネスの hook など) を退避し、テスト用の
        // 「previous」をセットする。default hook を残すと panic メッセージが
        // stderr に出てログを汚すので、ここで完全に差し替える。
        let saved = panic::take_hook();
        panic::set_hook(Box::new(|_info| {
            PREV_CALLS.fetch_add(1, Ordering::SeqCst);
        }));

        install_panic_hook_with(|| {
            RESTORE_CALLS.fetch_add(1, Ordering::SeqCst);
        });

        let result = panic::catch_unwind(AssertUnwindSafe(|| {
            panic!("intentional panic from test");
        }));

        // 退避していた hook に戻してから assertion する (assertion 失敗時に
        // 元の hook で panic メッセージを出したいため)。
        let _ = panic::take_hook();
        panic::set_hook(saved);

        assert!(result.is_err());
        assert_eq!(
            RESTORE_CALLS.load(Ordering::SeqCst),
            1,
            "restore は panic 時に 1 度呼ばれる"
        );
        assert_eq!(
            PREV_CALLS.load(Ordering::SeqCst),
            1,
            "previous hook も panic 時に 1 度呼ばれる"
        );
    }

    #[test]
    fn install_panic_hook_with_chains_multiple_layers() {
        let _g = HOOK_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        static OUTER_RESTORE: AtomicUsize = AtomicUsize::new(0);
        static INNER_RESTORE: AtomicUsize = AtomicUsize::new(0);
        static BASE_HOOK: AtomicUsize = AtomicUsize::new(0);
        OUTER_RESTORE.store(0, Ordering::SeqCst);
        INNER_RESTORE.store(0, Ordering::SeqCst);
        BASE_HOOK.store(0, Ordering::SeqCst);

        let saved = panic::take_hook();
        panic::set_hook(Box::new(|_| {
            BASE_HOOK.fetch_add(1, Ordering::SeqCst);
        }));

        // 1 段目を install (color_eyre 相当)
        install_panic_hook_with(|| {
            INNER_RESTORE.fetch_add(1, Ordering::SeqCst);
        });
        // 2 段目を install (こちらが「最後に呼ばれる set_hook」)
        install_panic_hook_with(|| {
            OUTER_RESTORE.fetch_add(1, Ordering::SeqCst);
        });

        let result = panic::catch_unwind(AssertUnwindSafe(|| panic!("layered panic")));

        let _ = panic::take_hook();
        panic::set_hook(saved);

        assert!(result.is_err());
        assert_eq!(OUTER_RESTORE.load(Ordering::SeqCst), 1, "外側の restore");
        assert_eq!(INNER_RESTORE.load(Ordering::SeqCst), 1, "内側の restore");
        assert_eq!(BASE_HOOK.load(Ordering::SeqCst), 1, "ベース hook も到達");
    }

    #[test]
    fn restore_is_safe_to_call_outside_raw_mode() {
        // 通常終了経路 (raw mode に入っていない) からの呼び出しでもパニック
        // しないことを確認する。crossterm の execute! / disable_raw_mode は
        // tty が無い CI 環境では失敗しうるが、restore 自身は握りつぶすため
        // 例外は外に漏れない。
        super::restore();
    }
}
