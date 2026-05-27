# DEPENDENCY_POLICY — 依存ライブラリ追従ポリシー

`ratatui` / `crossterm` / `reqwest` / `tokio` などコアの依存が major bump
した時に「壊さず追従する」ための運用手順を定める。

将来 `CONTRIBUTING.md` ([issue #3](https://github.com/astail/vozltop/issues/3)) が整備された段階で、
本ドキュメントは CONTRIBUTING.md からリンクされる予定。

---

## 適用範囲

Dependabot が `Cargo.toml` または `.github/workflows/*.yml` に対して
PR を出してきたとき、以下の分類で対応する。

| 種別 | 対応 |
|------|------|
| `patch` (例: 1.2.3 → 1.2.4) | CI green なら原則自動マージ。手動レビュー不要 |
| `minor` (例: 1.2.0 → 1.3.0) | CI green なら原則自動マージ。CHANGELOG をざっと眺めて気になる点があればコメント |
| `major` (例: 0.29 → 0.30, 1.x → 2.x) | **本ドキュメントの「major 手順」を必ず実施** |

`patch` / `minor` の自動マージは `.github/workflows/` の Dependabot automerge
ジョブが担当する（CI green が前提）。失敗時は手動 PR レビューに自然降格する。

---

## major 手順

以下を **PR ごとにチェックリスト化** して PR コメントに残す。担当者は 1 名アサインする。

### 1. アップストリームの破壊的変更を読む

- 公式 CHANGELOG / BREAKING-CHANGES.md / migration guide を読む
- 不明点は `context7` MCP で公式ドキュメントを引く
  - 例: `ratatui` → `/ratatui/ratatui/ratatui-v0.30.0` で BREAKING-CHANGES.md を取得
  - 例: `reqwest` → `/seanmonstar/reqwest` で migration セクションを参照
- **「自分のコードに影響しないとは限らない」前提で全項目に目を通す**

### 2. 影響範囲を grep で洗い出す

- 削除/改名された API を `rg` で全件検索し、ヒットするファイルを列挙
- `src/` 以下と `tests/` 以下の双方を必ず見る
- prelude が変わった場合は `use ratatui::prelude::*;` などの import 文も対象

### 3. ローカルで動作確認

- `cargo build --locked` で型エラーが無いことを確認
- `cargo test --locked` で全テスト緑
- `cargo clippy --all-targets --locked -- -D warnings` を通す
- TUI 部分は **Docker fixtures で実 nginx-vts を立てて起動**して挙動を確認

  ```bash
  docker run --rm -p 8080:80 -d xcgd/nginx-vts
  ab -n 5000 -c 50 http://localhost:8080/
  cargo run -- http://localhost:8080/status/format/json --interval 0.5
  ```

### 4. UI スナップショットの更新

- `insta` スナップショットテストが赤くなった場合、**差分が意図したものか目視確認**してから更新

  ```bash
  cargo insta review     # 1 件ずつ確認しながら accept/reject
  ```

- 差分の意図が説明できない場合は accept しない（バグの可能性）

### 5. PR コメントに残すサマリ

PR の最後に以下を貼る（テンプレ）:

```markdown
## Major bump checklist
- [x] BREAKING-CHANGES.md 読了
- [x] 影響範囲を grep で確認 (該当: <ファイル/行 を列挙>)
- [x] cargo build / test / clippy 緑
- [x] Docker fixtures で動作確認
- [x] insta snapshot 差分の妥当性確認
- 影響を受けた API: <list>
- 補正コミット: <link>
```

---

## 具体例: `ratatui` 0.29 → 0.30

参考までに、`ratatui-v0.30.0` で主要な破壊的変更:

- `List::start_corner(Corner::*)` 廃止 → `List::direction(ListDirection::*)`
- prelude から `Styled` / `symbols::Marker` / `terminal::{CompletedFrame, TerminalOptions, Viewport}` が外れた
  - 個別に `use ratatui::{style::Styled, ...}` を追記する必要あり
- `Backend` trait に `Error` 関連型と `clear_region` メソッドが追加
- `default-features` 無効化で layout cache も無効になる（性能影響あり）

詳細は <https://github.com/ratatui/ratatui/blob/ratatui-v0.30.0/BREAKING-CHANGES.md> を参照。

---

## MSRV 引き上げを伴う major bump

依存が `rust-version` を引き上げてきた場合は **必ず別 issue に分割**する:

- 本リポジトリの MSRV は `Cargo.toml` の `rust-version = "1.74"` で固定
- CI の MSRV ジョブ（issue #7）が走るので失敗時に検知できる
- MSRV を上げる判断は、依存追従の判断とは別軸（ユーザ環境への影響が大きい）

MSRV bump を含む PR は本ポリシーの「major 手順」に **加えて** 以下を行う:

1. README / CLAUDE.md の MSRV 記載を更新
2. 上げる動機（セキュリティ修正、エコシステムの収束、等）を PR 本文に明記
3. リリースノート (`CHANGELOG.md` の `Changed` セクション) に **必ず** 1 行追加

---

## 関連 issue / PR

- #7 MSRV (Rust 1.74) チェックを CI に追加
- #3 CONTRIBUTING.md（本ドキュメントをリンクする予定）
- #14 ratatui 0.30 系への追従可否を確認 (v1 実装前)
