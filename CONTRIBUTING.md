# Contributing to vozltop

vozltop へのコントリビューション、ありがとうございます！本ドキュメントは外部コントリビューターおよびメンテナ向けの開発ガイドです。

> このプロジェクトの全体方針 / アーキテクチャ / 実装順序は [`CLAUDE.md`](CLAUDE.md) と [`docs/DESIGN.md`](docs/DESIGN.md) を参照してください。本ドキュメントはそれらを「どう実行するか」に絞った手順書です。

## 1. 開発環境セットアップ

### 必須

| ツール | 推奨バージョン | 用途 |
|--------|---------------|------|
| Rust toolchain | stable (MSRV **1.74**) | コンパイル / `cargo test` |
| Git | 2.30+ | `git worktree` を使うため |
| Docker | 任意の最新 | nginx-vts fixture 取得 / E2E 動作確認 |

```bash
# Rust toolchain (stable + MSRV 確認)
rustup install stable
rustup install 1.74        # MSRV ビルド検証用

# 依存とビルド
git clone https://github.com/astail/vozltop
cd vozltop
cargo build
cargo test
```

### Docker fixture (任意)

実 nginx の vts レスポンスを使った確認は以下:

```bash
docker run --rm -p 8080:80 -d --name nginx-vts xcgd/nginx-vts
ab -n 5000 -c 50 http://localhost:8080/    # トラフィック生成
cargo run -- http://localhost:8080/status/format/json --interval 0.5
```

`tests/fixtures/*.json` は実 nginx-vts から取得したものです。手書きで差し替えないでください。

## 2. 実装順序

v1 の実装順序は [`CLAUDE.md` の "実装順序 (v1)" セクション](CLAUDE.md#実装順序v1) を参照してください。原則として **層の下から順** (model → client → state → event → ui) に進めます。

新規 issue を着手する前に該当ステップが完了しているか確認してください。

## 3. コミットメッセージ

[Conventional Commits](https://www.conventionalcommits.org/) を推奨します。

```
<type>(<scope>): <subject>

<body (optional)>

<footer (optional, e.g. closes #N)>
```

| type      | 用途                              |
|-----------|-----------------------------------|
| `feat`    | 新機能                            |
| `fix`     | バグ修正                          |
| `docs`    | ドキュメントのみの変更            |
| `test`    | テスト追加・修正                  |
| `refactor`| 挙動を変えないリファクタ          |
| `chore`   | ビルド / CI / 依存等              |
| `perf`    | パフォーマンス改善                |
| `style`   | フォーマットのみ                  |
| `revert`  | revert コミット                   |

**scope の例:** `ui`, `ui/table`, `client`, `state`, `cli`, `ci`, `security`, `deps`

**例:**

```
feat(ui/table): Server タブの p95 列を追加 (closes #28)
fix(client): タイムアウト時に正しく再試行する
docs(security): SECURITY.md と PVR 窓口を追加 (closes #38)
```

footer に `closes #N` / `fixes #N` / `resolves #N` を書いておくと、merge 時に対応 issue が自動クローズします。

## 4. PR 提出フロー

1. **issue を確認 / 作成** — 何を解決するか明確にしてから着手してください。labels (`area:*`, `type:*`, `phase:*`, `priority:*`) で優先度を判断します。
2. **ブランチを作成** — `git worktree add -b <prefix>-<scope> issue-<N> origin/main` を推奨 (並列作業を阻害しないため)。命名は `feat/<scope>-<short>` / `fix/<scope>-<short>` 等を推奨。
3. **実装 + テスト** — 変更行に対応する unit / integration / snapshot テストを追加。`cargo test` がローカルで pass することを確認。
4. **`cargo clippy --all-targets -- -D warnings` を pass** させる。
5. **`cargo fmt --check`** を pass させる。
6. **コミット** — Conventional Commits + `closes #N` フッタ。
7. **PR 作成** — テンプレートに沿って記入。`Development` セクションに対応 issue を紐づける (PR 本文の `closes #N` で自動)。
8. **CI green を確認** — fmt + clippy / test (ubuntu, macos) すべて pass。
9. **レビュー対応** — レビュー指摘は新しいコミットで対応 (force push しない)。

### 守ること / やらないこと

- ❌ `main` / `master` への force push
- ❌ `--no-verify` で hook をバイパス
- ❌ Dependabot の自動 PR を勝手にクローズ (詳細は #13 のポリシー参照)
- ✅ コンフリクトは rebase ではなく merge で解消 (履歴が読みやすい)
- ✅ UI 変更は `cargo insta review` で snapshot を更新

## 5. テストの方針

| 種別 | 場所 | 何を確認するか |
|------|------|----------------|
| Unit | `src/**/*.rs` の `#[cfg(test)] mod tests` | 純粋関数 / 派生メトリクス / CLI パース |
| Integration (deserialize) | `tests/deserialize.rs` | 実 nginx-vts JSON が壊れずパースできる |
| Integration (derived) | `tests/derived.rs` | RPS / p95 / hit% の計算が fixture と一致 |
| UI snapshot | `insta` | 4 状態 × 3 タブの描画差分 (#35) |

CI は `cargo test` / `cargo clippy -D warnings` / `cargo fmt --check` を ubuntu / macos の両 OS で実行します。Windows 対応は v1 スコープ外です。

## 6. issue / PR テンプレート

このリポジトリには以下のテンプレートが設定されています:

- `.github/ISSUE_TEMPLATE/bug_report.md` — バグ報告
- `.github/ISSUE_TEMPLATE/feature_request.md` — 機能要望
- `.github/ISSUE_TEMPLATE/config.yml` — 自由記述 issue は無効化、議論は GitHub Discussions へ
- `.github/PULL_REQUEST_TEMPLATE.md` — PR 概要 / 関連 issue / テスト方法

## 7. ライセンス

このリポジトリへのコントリビューションは [MIT License](LICENSE) のもとに公開されます。あなたのコミットが MIT 互換であることを暗黙的に同意したものとみなします。

## 8. 困ったら

- 設計判断に迷ったら [`CLAUDE.md` の "設計判断" 表](CLAUDE.md#設計判断変更前にユーザー確認) を参照
- 脆弱性の報告は [`SECURITY.md`](SECURITY.md) (public issue では報告しない)
- 質問は [GitHub Discussions](https://github.com/astail/vozltop/discussions) または既存 issue へコメント
