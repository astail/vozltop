//! TUI 用の配色 (`Theme`)。
//!
//! 配色は `Theme::color()` と `Theme::mono()` の 2 種類。`--no-color` フラグまたは
//! `NO_COLOR` 環境変数のいずれかがセットされていれば `mono()` を選ぶ
//! (issue #26、<https://no-color.org/> 準拠)。
//!
//! ## 設計判断
//! - **ANSI 16 色基準**: SSH 越しの古いターミナルや 256-color 非対応端末でも
//!   壊れないよう、`Color::Rgb` や `Color::Indexed` は使わない。`Color::Red` /
//!   `Color::Yellow` / ... のような名前付き色 (= 端末の theme に従う) のみ。
//! - **mono は強調を Modifier に寄せる**: foreground 色を持たない (= 端末の
//!   default 前景色を使う) 代わりに `BOLD` / `REVERSED` / `DIM` で違いを出す。
//!   エラーバナーは更に `[!]` プレフィックスで強調する (`error_banner_prefix`)。
//! - **保持先**: `App::theme` フィールドに 1 つだけ持つ。後続 issue (#27-#33) の
//!   UI モジュールは `app.theme.<field>` を直接参照する。

use ratatui::style::{Color, Modifier, Style};

use crate::cli::Args;

/// UI の配色をひとまとめにした構造体。
///
/// 各フィールドは ratatui の `Style` をそのまま保持する (UI 側で `.style(...)` に
/// 渡せばよい)。色追加が必要になったら本 struct にフィールドを追加して
/// `color()` / `mono()` の双方を更新する運用。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    /// アプリケーションのタイトル / ロゴ。
    pub title: Style,
    /// ヘッダーの "label:" 部分。
    pub header_label: Style,
    /// ヘッダーの数値部分 (rps / bw など)。
    pub header_value: Style,
    /// テーブルの 1 行目 (列名)。
    pub table_header: Style,
    /// 選択中のテーブル行。
    pub row_selected: Style,
    /// `AppStatus::Running` のバナー。
    pub status_ok: Style,
    /// `AppStatus::Stale` のバナー。
    pub status_warn: Style,
    /// `AppStatus::Disconnected` のバナー。
    pub status_err: Style,
    /// `app.error_banner` の描画スタイル。
    pub error_banner: Style,
    /// nginx 再起動検出バナー。
    pub restart_banner: Style,
    /// フッターのキーヒント部。
    pub footer: Style,
    /// 区切り線 / 装飾。
    pub separator: Style,
    /// 本テーマが mono かどうか。`error_banner_prefix` などの分岐に使う。
    pub mono: bool,
}

impl Theme {
    /// カラー配色。通常時 (= `--no-color` も `NO_COLOR` も未指定) のデフォルト。
    pub fn color() -> Self {
        Self {
            title: Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            header_label: Style::new().fg(Color::Gray),
            header_value: Style::new().fg(Color::White),
            table_header: Style::new()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD | Modifier::REVERSED),
            row_selected: Style::new().add_modifier(Modifier::REVERSED),
            status_ok: Style::new().fg(Color::Green).add_modifier(Modifier::BOLD),
            status_warn: Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            status_err: Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
            error_banner: Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
            restart_banner: Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            footer: Style::new().fg(Color::DarkGray),
            separator: Style::new().fg(Color::DarkGray),
            mono: false,
        }
    }

    /// モノクロ配色。`NO_COLOR` 環境変数 or `--no-color` で選択される。
    ///
    /// 前景色は一切持たず (= 端末 default を使う)、強調は `BOLD` / `REVERSED` /
    /// `DIM` の Modifier だけで表現する。エラー / 警告系は `BOLD` (+ `REVERSED`)
    /// を強めにすることで、色を持たなくても視認できるようにする。
    pub fn mono() -> Self {
        Self {
            title: Style::new().add_modifier(Modifier::BOLD),
            header_label: Style::new(),
            header_value: Style::new().add_modifier(Modifier::BOLD),
            table_header: Style::new().add_modifier(Modifier::BOLD | Modifier::REVERSED),
            row_selected: Style::new().add_modifier(Modifier::REVERSED),
            status_ok: Style::new().add_modifier(Modifier::BOLD),
            status_warn: Style::new().add_modifier(Modifier::BOLD),
            status_err: Style::new().add_modifier(Modifier::BOLD | Modifier::REVERSED),
            error_banner: Style::new().add_modifier(Modifier::BOLD | Modifier::REVERSED),
            restart_banner: Style::new().add_modifier(Modifier::BOLD),
            footer: Style::new().add_modifier(Modifier::DIM),
            separator: Style::new().add_modifier(Modifier::DIM),
            mono: true,
        }
    }

    /// `Args` から自動選択する。`--no-color` または `NO_COLOR` 環境変数 (非空) が
    /// あれば `mono()`、それ以外は `color()`。
    pub fn from_args(args: &Args) -> Self {
        Self::from_no_color(args.no_color_effective())
    }

    /// bool で明示的に切り替える低レベル版。テストや UI 切替で使う。
    pub fn from_no_color(no_color: bool) -> Self {
        if no_color {
            Self::mono()
        } else {
            Self::color()
        }
    }

    /// エラーバナーに付与する prefix。
    ///
    /// mono 時は色での強調ができないため、`"[!] "` を付けて視覚的に強調する
    /// (issue #26 の受け入れ条件)。color 時は空文字 (色だけで識別できる)。
    pub fn error_banner_prefix(&self) -> &'static str {
        if self.mono {
            "[!] "
        } else {
            ""
        }
    }
}

impl Default for Theme {
    /// デフォルトは `color()`。
    ///
    /// `App::default()` や `App::new()` がテーマ未指定で呼ばれた場合に使われる。
    /// CLI 経由のエントリ (`main.rs`) では `Theme::from_args(&args)` で
    /// `mono()` を選ぶケースもある。
    fn default() -> Self {
        Self::color()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `cli::tests::ScopedEnv` と同じ責務 (NO_COLOR を一時的に変更/復元)。
    ///
    /// cli モジュールの `ScopedEnv` は `#[cfg(test)]` の private 配下にあり
    /// 直接呼べないので、本 mod 用に再実装する。スレッド競合の許容理由も同様。
    struct ScopedEnv {
        key: &'static str,
        original: Option<std::ffi::OsString>,
    }
    impl ScopedEnv {
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
    impl Drop for ScopedEnv {
        fn drop(&mut self) {
            match &self.original {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }

    fn args_with(no_color: bool) -> Args {
        Args {
            url: url::Url::parse("http://x/s").unwrap(),
            interval: 1.0,
            user: None,
            headers: Vec::new(),
            insecure: false,
            no_color,
        }
    }

    #[test]
    fn color_has_foreground_colors() {
        let t = Theme::color();
        assert!(!t.mono);
        assert_eq!(t.title.fg, Some(Color::Cyan));
        assert_eq!(t.status_ok.fg, Some(Color::Green));
        assert_eq!(t.status_warn.fg, Some(Color::Yellow));
        assert_eq!(t.status_err.fg, Some(Color::Red));
        assert_eq!(t.error_banner.fg, Some(Color::Red));
    }

    #[test]
    fn mono_has_no_foreground_colors() {
        let t = Theme::mono();
        assert!(t.mono);
        // mono は前景色を持たない (端末 default 前景色を使う)
        for style in [
            t.title,
            t.header_label,
            t.header_value,
            t.table_header,
            t.row_selected,
            t.status_ok,
            t.status_warn,
            t.status_err,
            t.error_banner,
            t.restart_banner,
            t.footer,
            t.separator,
        ] {
            assert_eq!(style.fg, None, "mono theme must not set fg: {style:?}");
            assert_eq!(style.bg, None, "mono theme must not set bg: {style:?}");
        }
    }

    #[test]
    fn mono_uses_modifiers_for_emphasis() {
        // 色を持たない代わりに BOLD / REVERSED で違いを出していること。
        let t = Theme::mono();
        assert!(t.title.add_modifier.contains(Modifier::BOLD));
        assert!(t.table_header.add_modifier.contains(Modifier::REVERSED));
        assert!(t.row_selected.add_modifier.contains(Modifier::REVERSED));
        assert!(t.error_banner.add_modifier.contains(Modifier::REVERSED));
    }

    #[test]
    fn color_uses_only_ansi16_palette() {
        // ANSI 16 色のみ許可。Rgb / Indexed は禁止 (SSH 越しの古い端末対策)。
        fn is_ansi16(c: Color) -> bool {
            matches!(
                c,
                Color::Reset
                    | Color::Black
                    | Color::Red
                    | Color::Green
                    | Color::Yellow
                    | Color::Blue
                    | Color::Magenta
                    | Color::Cyan
                    | Color::Gray
                    | Color::DarkGray
                    | Color::LightRed
                    | Color::LightGreen
                    | Color::LightYellow
                    | Color::LightBlue
                    | Color::LightMagenta
                    | Color::LightCyan
                    | Color::White
            )
        }
        let t = Theme::color();
        for (name, style) in [
            ("title", t.title),
            ("header_label", t.header_label),
            ("header_value", t.header_value),
            ("table_header", t.table_header),
            ("row_selected", t.row_selected),
            ("status_ok", t.status_ok),
            ("status_warn", t.status_warn),
            ("status_err", t.status_err),
            ("error_banner", t.error_banner),
            ("restart_banner", t.restart_banner),
            ("footer", t.footer),
            ("separator", t.separator),
        ] {
            if let Some(c) = style.fg {
                assert!(is_ansi16(c), "{name}.fg uses non-ANSI16 color: {c:?}");
            }
            if let Some(c) = style.bg {
                assert!(is_ansi16(c), "{name}.bg uses non-ANSI16 color: {c:?}");
            }
        }
    }

    #[test]
    fn error_banner_prefix_is_bang_in_mono_and_empty_in_color() {
        assert_eq!(Theme::color().error_banner_prefix(), "");
        assert_eq!(Theme::mono().error_banner_prefix(), "[!] ");
    }

    #[test]
    fn default_theme_is_color() {
        let t = Theme::default();
        assert!(!t.mono);
        assert_eq!(t.title.fg, Some(Color::Cyan));
    }

    #[test]
    fn from_no_color_branches_on_flag() {
        assert!(!Theme::from_no_color(false).mono);
        assert!(Theme::from_no_color(true).mono);
    }

    #[test]
    fn from_args_picks_mono_when_flag_set() {
        let _env = crate::test_util::ENV_LOCK.lock().unwrap();
        let _guard = ScopedEnv::remove("NO_COLOR");
        let t = Theme::from_args(&args_with(true));
        assert!(t.mono);
    }

    #[test]
    fn from_args_picks_color_when_neither_set() {
        let _env = crate::test_util::ENV_LOCK.lock().unwrap();
        let _guard = ScopedEnv::remove("NO_COLOR");
        let t = Theme::from_args(&args_with(false));
        assert!(!t.mono);
    }

    #[test]
    fn from_args_picks_mono_when_env_set_non_empty() {
        let _env = crate::test_util::ENV_LOCK.lock().unwrap();
        let _guard = ScopedEnv::set("NO_COLOR", "1");
        let t = Theme::from_args(&args_with(false));
        assert!(t.mono);
    }

    #[test]
    fn from_args_picks_color_when_env_set_to_empty_string() {
        // https://no-color.org: "present and not an empty string"
        let _env = crate::test_util::ENV_LOCK.lock().unwrap();
        let _guard = ScopedEnv::set("NO_COLOR", "");
        let t = Theme::from_args(&args_with(false));
        assert!(!t.mono);
    }
}
