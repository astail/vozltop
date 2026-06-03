# Changelog

このプロジェクトの注目すべき変更はすべてこのファイルに記録されます。

フォーマットは [Keep a Changelog 1.1.0](https://keepachangelog.com/en/1.1.0/) に準拠し、
バージョニングは [Semantic Versioning 2.0.0](https://semver.org/spec/v2.0.0.html) に従います。

PR を出すときは、変更点を該当する [Unreleased](#unreleased) のサブセクション
（`Added` / `Changed` / `Deprecated` / `Removed` / `Fixed` / `Security`）に 1 行追加してください。
詳細は将来追加される `CONTRIBUTING.md` (issue #3) で扱います。

## [Unreleased]

## [0.4.0] - 2026-06-04

### Changed

- ヘッダと table を ratatui の rounded box (`╭╮╰╯`、`--no-color` / mono 環境では plain `┌┐└┘`) で囲み、table title を `{TabName} · {count} [· filter "q"]` 形式に変更。列見出しに `↓` / `↑` のソート方向矢印、カーソル行に `▶` マーカーを追加し、アラートや `5xx>0` の着色はセル単位に限定した。タイトル行には ● ステータスドット + host + uptime を表示する (closes #150) (#151)
- ヘッダの `RPS` / `IN` / `OUT` から bar 表示を撤廃し、数値のみの右寄せ表記に統一。詳細オーバーレイの histogram も横向き bar の描画を廃止して、3 セクション (`Latency` / `Status codes` / `Buckets`) のテキスト表記に整理した。`EXPIRED` ラベルが詰まる cell 幅で `PIRED` のように先頭が欠落する既知の挙動については追跡継続 (closes #152) (#153)
- 詳細オーバーレイの histogram から p95 バケットを示す `▶` マーカーを削除し、純粋なバケット分布として描画する (closes #147) (#148)

### Documentation

- README (日本語版 / 英語版) と `docs/DESIGN.md` を現状の TUI に合わせて更新。bar 撤廃後のヘッダ図、右寄せされた数値列、Filter タブの記述、rounded box の枠線などを反映した (closes #154)

### Security

- 依存クレート `ratatui` を 0.29 → 0.30 に更新。transitive で混入していた `paste 1.0.15` (RUSTSEC-2024-0436: unmaintained) と `lru 0.12.5` (RUSTSEC-2026-0002: `IterMut` Stacked Borrows 違反) を同時に解消する。`ratatui 0.30` で `paste` 依存は drop され、`lru` は patched 済みの 0.16.x にバンプされた。vozltop 側は `Sparkline::data(&[u64])` / `Paragraph::alignment(Alignment::*)` が 0.30 でも互換のためソース変更は不要 (#144, #145, #149)

## [0.3.1] - 2026-06-02

### Fixed

- リリース運用上の不整合を修正するためのメンテナンスリリース。v0.3.0 では Cargo.toml のバージョン bump と tag push のタイミングがずれたため、Homebrew tap (`astail/homebrew-tap`) 経由で `brew install astail/tap/vozltop` した場合に同梱バイナリの `--version` 表示やアーカイブ名と tag 表記が綺麗に揃わない問題が発生していた。v0.3.1 では Cargo.toml / Cargo.lock の version を 0.3.1 に揃えた状態で tag を切り直し、配布物 (`vozltop-0.3.1-<target>.tar.gz` / `.deb` / `.rpm`) を一括で再生成する

## [0.3.0] - 2026-06-01

### Added

- feat(ui/detail): histogram を横向きバーに置き換え (#138) (ASTEL)
- feat(ui/detail): Enter でも詳細オーバーレイを閉じられるようにする (#136) (#137) (ASTEL)

### Fixed

- fix(ui/detail): histogram バーを PDF 化する (#134) (#135) (ASTEL)

### Changed

- docs: fix README box alignment and add English translation (#133) (ASTEL)

## [0.2.1] - 2026-06-01

### Added

- Homebrew tap `astail/homebrew-tap` 経由でのインストールに対応。`brew install astail/tap/vozltop` で macOS arm64 / Linux x86_64 musl / Linux aarch64 musl の prebuilt バイナリが入る。tap リポジトリ側は本リポジトリの `packaging/homebrew/vozltop.rb.template` を元に手動更新する運用 (Phase 2 で release workflow から自動 bump 候補)

### Changed

- 依存クレート `toml` を 0.8 → 1.1 に更新。`toml::from_str` が toml 1.x で `parse` + `serde` の両 feature を要求するように分離されたため、`Cargo.toml` の features 指定を `["parse", "serde"]` に変更 (#127)
- 依存クレート `directories` を 5 → 6 に更新。`ProjectDirs::from` / `config_dir()` の API は互換のため呼び出し側コードの変更は無し (#126)
- GitHub Actions の release workflow で使うアクションを更新: `actions/upload-artifact` 4 → 7 (#123)、`softprops/action-gh-release` 2 → 3 (#124)、`actions/download-artifact` 4 → 8 (#125)
- `.github/dependabot.yml` の cargo セクションに ratatui の major bump (`version-update:semver-major`) を ignore するルールを追加。CLAUDE.md / `docs/notes/ratatui-030-evaluation.md` で v1 期間中は 0.29.x に固定する方針のため、毎週 0.30 への bump PR が再生成されるのを止める (#131)

### Fixed

- `vozltop --version` / `--help` が exit code 1 で color-eyre 経由のエラー表示になっていた問題を修正。issue #46 で `Args::parse()` から `Args::parse_with_config_from` (`try_parse_from` ベース) に切り替えた際に、clap が `--version` / `--help` を `Err(clap::Error)` (`ErrorKind::DisplayVersion` / `DisplayHelp`) で返すケースを正常情報表示として処理する分岐が抜けていた。これらの kind だけ `e.exit()` (stdout + exit 0) に委譲し、それ以外の parse error は従来通り `ConfigArgsError` 経由で main の color-eyre フォーマッタに流す。あわせて `tests/bin_smoke.rs` を新設し、ビルド済みバイナリの exit code を子プロセス起動で検証する (closes #129)

## [0.2.0] - 2026-06-01

### Added

- 複数 URL を引数に取って multi-host 監視に対応。ヘッダ上段が host タブ、下段が zone タブの 2 層構造になり、`[` / `]` (または `Shift+H` / `Shift+L`) で host を切り替えられる。fetch loop は host ごとに独立して走り、片側が落ちても他方の描画は継続する (closes #44)

### Changed

- ヘッダの `BW` 行を `in` / `out` の 2 行に分割し、sparkline をフル幅に拡張。従来の左右半々分割では sparkline の解像度が低く突発的なバースト (バースト幅 < 列幅) が潰れて見えなかったため、縦方向に冗長化して 1 行あたり全幅で in / out を独立描画する (closes #119)
- README のヘッダ図と説明を実装に合わせて更新 (`Conn` 行の Gauge、`RPS` 行の Sparkline、`BW` 行の `in`/`out` 2 行分割)

### Removed

- `--alert-5xx-pct` CLI フラグと、それに紐づく 5xx 行ハイライト / ベル機能を削除。ヘッダ 2 行目の `5xx N.NN%` 表示も削除。理由: 5xx は production の異常検知で重要だが、ヘッダ右への常時併記はノイズが多く、行レベルで個別に確認した方が情報の precision が高い。`--alert-p95-ms` (p95 レイテンシ閾値) と SLO 監視の経路は維持する。TOML config の `alert_5xx_pct` フィールドも削除 (`alert_p95_ms` は維持)

### Fixed

- ヘッダの RPS / BW in / BW out sparkline が `*` zone (server zone 全体集計) を含む全 zone を一括加算しており、合算値が実トラフィックの約 2 倍になっていた問題を修正。`*` zone は他 zone の合計と等価なので、sparkline 集計時にスキップする (closes #117)
- `cargo-deb` 3.x の CLI 仕様変更 (`--target-arch` → `--target <triple>`) に追随し、Linux パッケージ生成 (`.deb` / `.rpm`) の release workflow を修正

## [0.1.0] - 2026-05-31

初回リリース。`htop` ライクな TUI で nginx-module-vts JSON をリアルタイム可視化する CLI。

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

[unreleased]: https://github.com/astail/vozltop/compare/v0.4.0...HEAD
[0.4.0]: https://github.com/astail/vozltop/compare/v0.3.1...v0.4.0
[0.3.1]: https://github.com/astail/vozltop/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/astail/vozltop/compare/v0.2.1...v0.3.0
[0.2.1]: https://github.com/astail/vozltop/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/astail/vozltop/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/astail/vozltop/releases/tag/v0.1.0
