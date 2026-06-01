# ratatui 0.30 採用評価 (2026-05)

issue [#14](https://github.com/astail/vozltop/issues/14) の調査結果と判断を記録する。後で 0.30 へ引き上げる際に再評価しやすくするためのメモ。

> **2026-06-02 追記**: 本評価当時の「v1 期間中は 0.29 維持」結論は撤回し、ratatui を **0.30** に bump 済み (#144 / #145 で同時対応)。動機は RUSTSEC-2024-0436 (paste unmaintained) / RUSTSEC-2026-0002 (lru `IterMut` unsoundness) の解消。実際に移行してみると、本評価で挙げていた breaking のうち vozltop が触れているもの (`Alignment` / `Sparkline::data(&[u64])` / `Table::new(rows, widths)`) はすべて 0.30 でも 0.29 互換に動作するため、**ソース側の変更は不要**だった。`Cargo.toml` の 1 行 bump (+ Cargo.lock + dependabot.yml の ignore 削除) のみ。

> **2026-05-27 補足**: 本評価当時に存在した「MSRV 1.74 vs 0.30 要求 MSRV 1.86 の衝突」という制約は、issue [#79](https://github.com/astail/vozltop/issues/79) で MSRV 宣言・CI ジョブを drop したため**失効**しています。

## TL;DR

**~~vozltop v1 期間中は ratatui 0.29 系を採用する。0.30 への追従は別 PR + 再評価で行う。~~** (撤回: 2026-06-02。RustSec advisory 解消のため 0.30 へ bump 済み)

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

## 0.30 へ移行する際のチェックリスト (2026-06-02 実施済み)

`#144` / `#145` 対応で実施した移行手順 (チェック結果は実 PR の diff を参照):

- [x] `Cargo.toml` の `ratatui = "0.29"` → `"0.30"`
- [x] `Table::new(rows).widths(widths)` → `Table::new(rows, widths)` の機械置換 → vozltop 側は元から 2 引数形式で利用しており**変更不要**
- [x] `Sparkline::data(&[u64])` 呼び出しの動作確認 → `From<u64> for SparklineBar` 経由でそのままコンパイル通過
- [x] `layout::Alignment` の参照 → 0.30 でも `Alignment` エイリアスが残っており**変更不要**
- [x] TestBackend / insta snapshot の更新差分を `cargo insta review` → 差分なし
- [x] `cargo clippy --all-targets -- -D warnings` を pass
- [x] `cargo fmt --check` を pass
- [x] `.github/dependabot.yml` の `ratatui major bump ignore` ルールを削除

参考: [BREAKING-CHANGES.md](https://github.com/ratatui/ratatui/blob/ratatui-v0.30.0/BREAKING-CHANGES.md)

## 参考リンク

- ratatui 0.30 BREAKING-CHANGES.md (上記)
- ratatui examples (公式) — Gauge / Table 等のスニペット
- vozltop CONTRIBUTING.md `## 5. テストの方針 > MSRV 引き上げのルール`
- vozltop docs/DEPENDENCY_POLICY.md
