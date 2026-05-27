---
name: Bug report
about: 動作がおかしい / 期待と違う場合の報告
title: 'bug: '
labels: ['type:bug']
assignees: ''
---

<!-- セキュリティ上の問題は public issue ではなく SECURITY.md の手順に従ってください。 -->

## 概要

<!-- 何が起きたか 1〜3 行で。 -->

## 再現手順

<!--
1. `vozltop http://localhost:8080/status/format/json --interval 0.5` を実行
2. ...
3. ...
-->

1.
2.
3.

## 期待される挙動

<!-- 本来こうなってほしい、という説明。 -->

## 実際の挙動

<!-- 実際に起きた挙動 / エラーメッセージ / panic backtrace 等。 -->

```
```

## 環境

<!-- 該当する内容を埋めてください。 -->

- vozltop バージョン (`vozltop --version` の出力):
- OS:
- ターミナル / マルチプレクサ (例: iTerm2 / Alacritty / tmux):
- nginx-module-vts バージョン:
- `vhost_traffic_status_histogram_buckets` 設定の有無:

## 添付資料

<!-- 可能なら以下を添付してください -->

- [ ] `/status/format/json` の生レスポンス (機微情報は伏せて)
- [ ] スクリーンショット / asciinema
- [ ] `RUST_BACKTRACE=1` 付きの実行ログ
