# ratatui 0.30 採用評価 (2026-05)

issue [#14](https://github.com/astail/vozltop/issues/14) の調査結果と判断を記録する。後で MSRV を引き上げる際に再評価しやすくするためのメモ。

## TL;DR

**vozltop v1 期間中は ratatui 0.29 系を採用する。0.30 への追従は MSRV を 1.86+ に引き上げる別 PR と同時に行う。**

## 評価対象

- 起点: ratatui **0.30.0** (2025 リリース予定 / 評価時 unreleased タグ `ratatui-v0.30.0`)
- 比較対象: ratatui **0.29.0**
- vozltop が利用予定の widget: `Sparkline`, `Gauge`, `Table`, `BarChart`, `TestBackend`

参考: [BREAKING-CHANGES.md @ ratatui-v0.30.0](https://github.com/ratatui/ratatui/blob/ratatui-v0.30.0/BREAKING-CHANGES.md)

## 主な変更点 (vozltop に影響)

### 🛑 MSRV: 1.86.0

> v0.30.0 Unreleased: ... the Minimum Supported Rust Version (MSRV) is now 1.86.0.

vozltop の MSRV は `Cargo.toml` の `rust-version = "1.74"` で固定されており、CI の `msrv (1.74)` ジョブ ([#7](https://github.com/astail/vozltop/issues/7)) で強制される。**1.86 への引き上げは独立 PR + issue 議論が必要** ([CONTRIBUTING.md "MSRV 引き上げのルール"](../../CONTRIBUTING.md))。

### Sparkline

- `Sparkline::data()` のシグネチャ変更:
  - 0.29: `fn data(self, data: &[u64]) -> Self`
  - 0.30: `fn data(self, data: impl IntoIterator<Item = SparklineBar>) -> Self`
- `SparklineBar` は `value: Option<u64>` を持ち、欠損データを「0」と区別できる
- `From` impls 多数なので `&[u64]` のままで概ね動くが、`const fn` ではなくなる点に注意

→ vozltop の `RPS / 5xx / BW Sparkline` ([#27](https://github.com/astail/vozltop/issues/27)) では欠損表現の恩恵あり。ただし採用しなくても既存 `&[u64]` で問題なし。

### Table

- `Table::new(rows)` → **`Table::new(rows, widths)`** に変更 (widths が必須引数化)
- `Table::widths()` の引数が `&[Constraint]` → `IntoIterator<Item = Constraint>` (slice 借用不要)

→ 影響: 機械的置換可能。むしろ widths を builder の連鎖から外せるので可読性向上。

### Gauge

- 評価時点の BREAKING-CHANGES に固有変更見当たらず。API はほぼ同等

→ 影響なし。

### BarChart

- 主立った breaking なし (`SparklineBar` 系の波及はあるかもしれないが直接 API は変わっていない)

→ 影響軽微。

### TestBackend

- `From` impls for backend types replaced with more specific traits (型安全性向上)

→ insta snapshot の `Terminal::with_options(...TestBackend::new(...))` 系は型変換が暗黙化していない場合がある。実装時に確認。

### その他の影響可能性

- `block::Title` removed → 直接利用は `Block::bordered().title("...")` ベースなので無関係
- `layout::Alignment` → `layout::HorizontalAlignment` リネーム
- `List::highlight_symbol` が `Into<Line>` 受け取り
- `Flex::SpaceAround` セマンティクス変更 (flexbox 準拠) → vozltop は未使用

## 判断: 0.29 維持

| 観点 | 0.30 採用 | 0.29 維持 |
|------|-----------|-----------|
| MSRV 1.86 制約 | ❌ vozltop 既定 (1.74) と衝突 | ✅ 1.74 で動く |
| API 改善 | ✅ Table 引数 / SparklineBar 欠損 | ⚠️ 0.29 だが致命的不足なし |
| 移行コスト | 機械的置換 + テスト確認 | 0 |
| Dependabot 自動追従 | ❌ major bump で停止 | ✅ 0.29.x patch は自動 |
| 後で 0.30 へ上げる難易度 | n/a | ⚠️ widget コードを全箇所書き換え |

`v1` 期間中は **0.29 系で固定** し、Dependabot は 0.29.x の patch のみ追従する。
0.30 への追従は以下の条件が揃ったタイミングで再評価する:

1. vozltop の MSRV を 1.86+ に引き上げる議論が独立に発生したとき
2. 0.30.x が複数 patch を出して安定したと判断できたとき
3. SparklineBar の欠損表現が必要な機能要望が出たとき (例: 観測中断時の Sparkline 表現)

## Cargo.toml バージョン制約 (UI 実装時の指針)

ratatui を最初に追加する PR (issue [#27](https://github.com/astail/vozltop/issues/27) を想定) では以下を採用:

```toml
[dependencies]
ratatui = { version = "0.29", default-features = false, features = ["crossterm"] }
crossterm = { version = "0.29", features = ["event-stream"] }
```

- `version = "0.29"` で 0.30 への自動上げを抑止 (cargo の semver は 0.x は minor で breaking 扱い)
- 必要に応じて `=0.29.0` まで pin することも検討するが、patch は追従したいので `"0.29"` を推奨
- `default-features = false` + `crossterm` のみで `termion` / `termwiz` を除外しビルドサイズを抑える

## 0.30 へ移行する際のチェックリスト (将来用)

将来 MSRV を 1.86+ にした後、以下を順に実施する:

- [ ] `Cargo.toml` の `ratatui = "0.29"` → `"0.30"`
- [ ] `Table::new(rows).widths(widths)` → `Table::new(rows, widths)` を `rg` で機械置換
- [ ] `Sparkline::data(&[u64])` 呼び出しの動作確認 (`SparklineBar::from(u64)` 経由)
- [ ] `layout::Alignment` の参照を `HorizontalAlignment` へリネーム
- [ ] TestBackend / insta snapshot の更新差分を `cargo insta review`
- [ ] `cargo clippy --all-targets -- -D warnings` を pass

参考: [BREAKING-CHANGES.md](https://github.com/ratatui/ratatui/blob/ratatui-v0.30.0/BREAKING-CHANGES.md) を毎回必ず参照すること。

## 参考リンク

- ratatui 0.30 BREAKING-CHANGES.md (上記)
- ratatui examples (公式) — Gauge / Table 等のスニペット
- vozltop CONTRIBUTING.md `## 5. テストの方針 > MSRV 引き上げのルール`
- vozltop docs/DEPENDENCY_POLICY.md
