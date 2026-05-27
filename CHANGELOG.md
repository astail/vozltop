# Changelog

このプロジェクトの注目すべき変更はすべてこのファイルに記録されます。

フォーマットは [Keep a Changelog 1.1.0](https://keepachangelog.com/en/1.1.0/) に準拠し、
バージョニングは [Semantic Versioning 2.0.0](https://semver.org/spec/v2.0.0.html) に従います。

PR を出すときは、変更点を該当する [Unreleased](#unreleased) のサブセクション
（`Added` / `Changed` / `Deprecated` / `Removed` / `Fixed` / `Security`）に 1 行追加してください。
詳細は将来追加される `CONTRIBUTING.md` (issue #3) で扱います。

## [Unreleased]

### Added

- 依存ライブラリの major bump 追従手順を `docs/DEPENDENCY_POLICY.md` に明文化 (#13)

### Changed

- （既存機能の変更をここに追加）

### Deprecated

- （将来削除される機能をここに追加）

### Removed

- （削除した機能をここに追加）

### Fixed

- （バグ修正をここに追加）

### Security

- `.github/workflows/audit.yml` で cargo-audit (RustSec advisory DB) を CI に追加。週次 + main push + Cargo.lock 変更時にスキャン (#8)

[unreleased]: https://github.com/astail/vozltop/commits/main
