//! 手動検証用: panic 時にターミナルが復旧することを確認する examples バイナリ。
//!
//! 使い方:
//!
//! ```sh
//! cargo run --example panic_terminal
//! ```
//!
//! 期待挙動:
//! - 約 1 秒間 alternate screen に切り替わり、カーソルが隠れる
//! - その後 panic が発生する
//! - シェルに戻ったときに以下が満たされていれば OK:
//!   - プロンプトが普通に表示される (raw mode が解除されている)
//!   - 入力した文字が echo される (raw mode が解除されている)
//!   - カーソルが見える (`Show` が走った)
//!
//! 復旧が壊れている場合、`reset` または `stty sane` を打たないと shell が
//! 戻らない。ssh 越しの運用ではこれが大事故になりうるので CI ではなく必ず
//! 手元で 1 度確認する (詳細は CONTRIBUTING.md "Panic hook の手動検証" を参照)。

use std::io::{self, Write};
use std::thread;
use std::time::Duration;

use crossterm::cursor::Hide;
use crossterm::execute;
use crossterm::terminal::{enable_raw_mode, EnterAlternateScreen};

fn main() -> color_eyre::eyre::Result<()> {
    color_eyre::install()?;
    vozltop::terminal::install_panic_hook();

    enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen, Hide)?;

    let mut out = io::stdout();
    write!(
        out,
        "intentional panic in 1s ... (terminal should restore after exit)"
    )?;
    out.flush()?;
    thread::sleep(Duration::from_secs(1));

    panic!("intentional panic for terminal-restore verification");
}
