<!--
PR テンプレート: 該当しないセクションは削除して構いません。
コミットメッセージは Conventional Commits を推奨します。
詳細は CONTRIBUTING.md を参照してください。
-->

## 概要

<!-- 何を解決する PR か、1〜3 行で説明してください。 -->

## 関連 issue

<!-- closes #N / fixes #N / resolves #N を記入すると merge 時に自動でクローズします。 -->

- closes #

## 変更内容

<!-- 主な変更点を箇条書きで。差分が大きい場合は分割を検討してください。 -->

-

## テスト方法

<!--
ローカルで実施したテスト手順 / コマンドを記載してください。
例:
- `cargo test --all-targets`
- `cargo clippy --all-targets -- -D warnings`
- `cargo run -- http://localhost:8080/status/format/json` で目視確認
-->

```
```

## スクリーンショット (UI 変更時)

<!-- TUI / ドキュメントの見た目を変える PR では before/after をスクリーンショットで添付してください。 -->

| before | after |
|--------|-------|
|        |       |

## チェックリスト

- [ ] `cargo test` がローカルで pass する
- [ ] `cargo clippy --all-targets -- -D warnings` が pass する
- [ ] `cargo fmt --check` が pass する
- [ ] (UI 変更時) `cargo insta review` で snapshot を更新済み
- [ ] (依存追加時) major bump の場合は CHANGELOG / migration guide を確認済み (#13 参照)
- [ ] PR タイトルが Conventional Commits 形式 (`feat(scope): ...` 等)
