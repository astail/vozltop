//! tokio interval / fetch task / crossterm `EventStream` / `ctrl_c` を
//! 単一の `AppEvent` ストリームに正規化する小さなレイヤ。
//!
//! 本ファイルは「正規化関数」のみを提供し、`tokio::select!` ループ自体は
//! issue #25 (`main.rs`) で組み立てる。設計上の理由:
//!
//! - 正規化関数 `map_event` を切り出すと、I/O を介さず純関数として
//!   テスト可能 (Windows/kitty 由来の Release/Repeat キーや、未対応の
//!   Mouse/Focus/Paste を v1 でどう扱うかをここに固める)。
//! - ループ本体は terminal の init/restore も伴うのでテストしにくく、
//!   別 issue (#25) に切り分ける方が PR レビュー単位が小さくなる。

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::client::FetchError;
use crate::model::VtsStatus;

/// アプリケーションが消費するイベント。
///
/// バリアントは issue #24 のスペックに従う:
///
/// - `Tick(Box<VtsStatus>)`: fetch ループが成功したときに渡される最新 snapshot。
///   `Box` で包む理由は `VtsStatus` が大きく (zone 群を含むため)、enum 全体の
///   サイズが膨らむと `tokio::select!` の各 branch で毎フレーム move コストが
///   効くため。
/// - `FetchErr(FetchError)`: fetch ループの失敗。`App::on_fetch_err` に渡す。
/// - `Key(KeyEvent)`: terminal からのキー入力 (Press のみ、Quit 系を除く)。
/// - `Resize(u16, u16)`: terminal リサイズ通知 (新しい cols, rows)。
/// - `Quit`: `tokio::signal::ctrl_c` または `q` / `Ctrl+C` / `F10` キー。
pub enum AppEvent {
    Tick(Box<VtsStatus>),
    FetchErr(FetchError),
    Key(KeyEvent),
    Resize(u16, u16),
    Quit,
}

impl std::fmt::Debug for AppEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // Tick の中身は zone 数によっては数百行になるので、Debug 出力は
            // 「Tick(..)」に抑える (ログを汚さない)。
            AppEvent::Tick(_) => f.write_str("Tick(..)"),
            // FetchError は Debug が URL を含みうるため、必ず
            // banner_message() (URL/secret を含まない) を経由する。
            AppEvent::FetchErr(err) => write!(f, "FetchErr({})", err.banner_message()),
            AppEvent::Key(k) => write!(f, "Key({k:?})"),
            AppEvent::Resize(w, h) => write!(f, "Resize({w}, {h})"),
            AppEvent::Quit => f.write_str("Quit"),
        }
    }
}

/// crossterm の `Event` を `AppEvent` に正規化する。
///
/// 戻り値が `None` の場合は「v1 では使わないので捨てる」イベント:
///
/// - `Event::Mouse` / `Event::FocusGained` / `Event::FocusLost` / `Event::Paste`
/// - キーの Release / Repeat (Windows / kitty keyboard protocol で発生)
///
/// `Some(AppEvent::Quit)` を返す条件は CLAUDE.md キー割り当てに従い
/// `q` (大文字小文字問わず) / `Ctrl+C` / `F10`。
///
/// ※ issue #24 の原文シグネチャは `-> AppEvent` だが、Mouse/Focus/Paste を
/// 漏らさず捨てるには `Option` 戻り値が最小の整合解。enum に `Ignored` 等の
/// 新バリアントを足すと `App` 側で取扱を強制されてしまい、UI レイヤを汚す。
pub fn map_event(ev: Event) -> Option<AppEvent> {
    match ev {
        Event::Key(k) => map_key(k),
        Event::Resize(cols, rows) => Some(AppEvent::Resize(cols, rows)),
        Event::Mouse(_) | Event::FocusGained | Event::FocusLost | Event::Paste(_) => None,
    }
}

fn map_key(k: KeyEvent) -> Option<AppEvent> {
    // 既定の Unix terminal では `KeyEventKind` は常に Press。
    // Windows / kitty keyboard protocol を有効化したときに Release/Repeat も
    // 飛ぶようになるが、v1 のショートカットは Press のみで成立するので絞る。
    if !matches!(k.kind, KeyEventKind::Press) {
        return None;
    }
    let is_ctrl_c = matches!(k.code, KeyCode::Char('c') | KeyCode::Char('C'))
        && k.modifiers.contains(KeyModifiers::CONTROL);
    if is_ctrl_c {
        return Some(AppEvent::Quit);
    }
    match k.code {
        KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::F(10) => Some(AppEvent::Quit),
        _ => Some(AppEvent::Key(k)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEventKind, KeyEventState, MouseButton, MouseEvent, MouseEventKind};

    fn press(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: mods,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn release(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Release,
            state: KeyEventState::NONE,
        }
    }

    // ---------- Key → Quit ----------

    #[test]
    fn q_key_maps_to_quit() {
        let ev = Event::Key(press(KeyCode::Char('q'), KeyModifiers::NONE));
        assert!(matches!(map_event(ev), Some(AppEvent::Quit)));
    }

    #[test]
    fn shift_q_also_maps_to_quit() {
        // SHIFT を伴うと crossterm は KeyCode::Char('Q') を返す環境がある。
        let ev = Event::Key(press(KeyCode::Char('Q'), KeyModifiers::SHIFT));
        assert!(matches!(map_event(ev), Some(AppEvent::Quit)));
    }

    #[test]
    fn ctrl_c_maps_to_quit() {
        let ev = Event::Key(press(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(matches!(map_event(ev), Some(AppEvent::Quit)));
    }

    #[test]
    fn ctrl_shift_c_also_quits() {
        // Ctrl+Shift+C は端末によって 'C' になる
        let ev = Event::Key(press(
            KeyCode::Char('C'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ));
        assert!(matches!(map_event(ev), Some(AppEvent::Quit)));
    }

    #[test]
    fn f10_maps_to_quit() {
        let ev = Event::Key(press(KeyCode::F(10), KeyModifiers::NONE));
        assert!(matches!(map_event(ev), Some(AppEvent::Quit)));
    }

    #[test]
    fn lone_c_without_ctrl_is_not_quit() {
        let ev = Event::Key(press(KeyCode::Char('c'), KeyModifiers::NONE));
        match map_event(ev) {
            Some(AppEvent::Key(k)) => assert_eq!(k.code, KeyCode::Char('c')),
            other => panic!("expected Key('c'), got {other:?}"),
        }
    }

    // ---------- Key → Key passthrough ----------

    #[test]
    fn arrow_keys_passthrough() {
        let ev = Event::Key(press(KeyCode::Down, KeyModifiers::NONE));
        match map_event(ev) {
            Some(AppEvent::Key(k)) => assert_eq!(k.code, KeyCode::Down),
            other => panic!("expected Key(Down), got {other:?}"),
        }
    }

    #[test]
    fn tab_passthrough_for_zone_switch() {
        let ev = Event::Key(press(KeyCode::Tab, KeyModifiers::NONE));
        match map_event(ev) {
            Some(AppEvent::Key(k)) => assert_eq!(k.code, KeyCode::Tab),
            other => panic!("expected Key(Tab), got {other:?}"),
        }
    }

    #[test]
    fn enter_passthrough_for_detail_overlay() {
        let ev = Event::Key(press(KeyCode::Enter, KeyModifiers::NONE));
        match map_event(ev) {
            Some(AppEvent::Key(k)) => assert_eq!(k.code, KeyCode::Enter),
            other => panic!("expected Key(Enter), got {other:?}"),
        }
    }

    #[test]
    fn release_event_is_ignored() {
        let ev = Event::Key(release(KeyCode::Char('q')));
        assert!(map_event(ev).is_none());
    }

    // ---------- Resize ----------

    #[test]
    fn resize_is_mapped_to_resize() {
        let ev = Event::Resize(120, 40);
        match map_event(ev) {
            Some(AppEvent::Resize(w, h)) => {
                assert_eq!(w, 120);
                assert_eq!(h, 40);
            }
            other => panic!("expected Resize, got {other:?}"),
        }
    }

    // ---------- Ignored events ----------

    #[test]
    fn mouse_events_are_ignored() {
        let ev = Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        });
        assert!(map_event(ev).is_none());
    }

    #[test]
    fn focus_events_are_ignored() {
        assert!(map_event(Event::FocusGained).is_none());
        assert!(map_event(Event::FocusLost).is_none());
    }

    #[test]
    fn paste_event_is_ignored() {
        let ev = Event::Paste("pasted".to_string());
        assert!(map_event(ev).is_none());
    }

    // ---------- Debug 出力 (機密情報リーク防止) ----------

    #[test]
    fn debug_tick_does_not_dump_payload() {
        // VtsStatus の Debug は zone 全部を出すので、AppEvent::Tick の Debug は
        // 「Tick(..)」に絞ってログを汚さない。
        let raw = serde_json::json!({
            "hostName": "h", "nginxVersion": "1", "moduleVersion": "v",
            "loadMsec": 0u64, "nowMsec": 1u64,
            "connections": {
                "active": 0, "reading": 0, "writing": 0,
                "waiting": 0, "accepted": 0, "handled": 0, "requests": 0
            },
        });
        let status: VtsStatus = serde_json::from_value(raw).unwrap();
        let ev = AppEvent::Tick(Box::new(status));
        assert_eq!(format!("{ev:?}"), "Tick(..)");
    }

    #[test]
    fn debug_fetch_err_uses_banner_message_not_raw_debug() {
        // FetchError::Connect の Debug は内部の reqwest::Error 経由で URL を
        // 含みうるため、AppEvent::FetchErr の Debug は banner_message を使う。
        use reqwest::StatusCode;
        let ev = AppEvent::FetchErr(FetchError::Status {
            code: StatusCode::INTERNAL_SERVER_ERROR,
        });
        assert_eq!(
            format!("{ev:?}"),
            "FetchErr(HTTP 500 Internal Server Error)"
        );
    }
}
