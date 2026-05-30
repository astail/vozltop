# Changelog

このプロジェクトの注目すべき変更はすべてこのファイルに記録されます。

フォーマットは [Keep a Changelog 1.1.0](https://keepachangelog.com/en/1.1.0/) に準拠し、
バージョニングは [Semantic Versioning 2.0.0](https://semver.org/spec/v2.0.0.html) に従います。

PR を出すときは、変更点を該当する [Unreleased](#unreleased) のサブセクション
（`Added` / `Changed` / `Deprecated` / `Removed` / `Fixed` / `Security`）に 1 行追加してください。
詳細は将来追加される `CONTRIBUTING.md` (issue #3) で扱います。

## [Unreleased]

### Added

- TOML 設定ファイル (`~/.config/vozltop/config.toml` 他 XDG ベース) と `vozltop @<alias>` 形式のエイリアス起動をサポート。`[hosts.<alias>]` セクションに URL / user / interval / headers / insecure / no_color / alert 閾値を書いて再利用できる。`--config <path>` フラグまたは `$VOZLTOP_CONFIG` 環境変数で明示指定も可能。CLI フラグは config を上書き (CLI > config[hosts.<alias>] > 組み込み既定)。keyring 連携は本 PR スコープ外で、password は config に平文 (chmod 0600 推奨) (closes #46)
- Linux パッケージ (`.deb` / `.rpm`) を tag push 時に GitHub Release へ自動添付するよう `release.yml` に `packaging` job を追加。`cargo-deb` と `cargo-generate-rpm` を利用し、`x86_64` / `aarch64` の 2 アーキテクチャをサポート。あわせて Cargo.toml に `[package.metadata.deb]` / `[package.metadata.generate-rpm]` を追加 (closes #49)
- Homebrew formula のテンプレート `packaging/homebrew/vozltop.rb.template` を staging。`astail/homebrew-tap` への反映は手動 (Phase 2 で自動化候補)
- README にインストール手順 (tarball / .deb / .rpm / Homebrew) を追加
- 依存ライブラリの major bump 追従手順を `docs/DEPENDENCY_POLICY.md` に明文化 (#13)
- `--user user:-` で stdin から password を読み取れるようにした (#40)
- `--header @path/to/file` でファイルからヘッダ 1 行を読み込めるようにした (#40)
- `VOZLTOP_PASSWORD` 環境変数で `--user` の password を上書きできるようにした (#40)

### Changed

- （既存機能の変更をここに追加）

### Deprecated

- （将来削除される機能をここに追加）

### Removed

- MSRV (Rust 1.74) 宣言を `Cargo.toml` から削除、CI の `msrv (1.74)` ジョブを廃止。stable toolchain のみをサポートする方針に変更 (closes #79)

### Fixed

- `Tab` / `Shift+Tab` で zone 種別タブ (Server / Upstream / Cache / Filter) を循環できるようにした。これまでヘルプ / footer は案内していたが key handler 側が未実装で Server タブから動かせなかった (closes #104)
- ヘッダの RPS / BW in / BW out sparkline が常時空のままだった問題を修正。`App::on_fetch_ok` が `derived::compute(prev, now)` を呼んでおらず `History::push_derived` も発火しなかったため、production 上で sparkline 3 本と現値表示が永続的に 0 になっていた。直前 snapshot との差分から派生メトリクスを算出し、serverZones を集計した合算値で sparkline を更新するよう修正 (closes #105)
- Cache タブで数字キー (1-8) / F5 のソート操作が表示に反映されない問題を修正。`render_cache` が `App::sort` を読まず `sort_cache_rows_default` (HIT% 降順固定) しか呼んでいなかったため、footer の `Sort: MISS ↑` 等が表示されても並び順は変わらない不整合があった。`sort_cache_rows(rows, sort)` を新規実装し、Cache タブの 8 列 (ZONE / HIT% / MISS / EXPIRED / STALE / USED / IN/s / OUT/s) すべてに動的ソートを対応。USED は比率 (used_size / max_size) 基準で並べ、`max_size == 0` は末尾。あわせて `selected_zone` の Cache 分岐を実装し、Enter で Cache zone の詳細オーバーレイが開けるようにした (closes #106)

### Security

- `.github/workflows/audit.yml` で cargo-audit (RustSec advisory DB) を CI に追加。週次 + main push + Cargo.lock 変更時にスキャン (#8)
- argv に password / Bearer トークンが平文で乗っていると起動時に stderr で警告するようにした (`ps` 経由の漏洩を防ぐ案内) (#40)
- URL に埋め込んだ credentials (`https://user:pass@host/...`) を `detect_argv_secret_in` の警告対象に追加。これまで `--user` / `--header` しか検査しておらず、URL 内 password が `ps` で漏洩しても無警告だった。URL 形式は `VOZLTOP_PASSWORD` でも上書きされないため env がセットされていても警告する (closes #107)

[unreleased]: https://github.com/astail/vozltop/commits/main
