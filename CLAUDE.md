# CLAUDE.md — vozltop

このファイルは Claude Code が本リポジトリで作業するときに毎回参照するプロジェクトコンテキストです。

## プロジェクト概要

**vozltop** は [vozlt/nginx-module-vts](https://github.com/vozlt/nginx-module-vts) が公開する nginx トラフィック統計 JSON を、`htop` のようなインタラクティブ TUI でリアルタイム可視化する CLI ツールです。

- 言語: **Rust**（stable、MSRV 1.74）
- 配布: 単一バイナリ（`cargo install vozltop` / GitHub Releases）
- ターゲット: linux x86_64/arm64, macOS arm64
- ライセンス: 未確定（v1 リリース前に MIT を予定）

## 設計判断（変更前にユーザー確認）

| 項目 | 決定 |
|------|------|
| レイアウト | htop風（上部サマリ + 下部 1 つの並び替え可能テーブル、Tab で zone 種別切替） |
| v1 サポート zone | `serverZones` / `upstreamZones` / `cacheZones`（`filterZones` は Phase 2） |
| 認証 | デフォルト無し。`--user user:pass` / `--header 'K: V'` / `--insecure` をオプション提供 |
| 書き込み系 (`/status/control`) | v1 はサポートしない（read-only） |
| レイテンシ表示 | テーブルに p95、Enter の詳細ビューで p50/p95/p99 + histogram |
| Upstream 行の粒度 | 1 行 = 1 server（`ZONE` 列は `group/host:port` 表記）。group 集約は Phase 2 |
| histogram なし zone | p95 列を `~Nms`（平均値）表示。p95 ソート時は histogram あり zone より下位に固定 |
| 接続数 Gauge | 観測中の rolling-max で auto-scale（`worker_connections` は JSON から取れないため） |
| 設定ファイル | v1 では持たない（CLI 引数のみ） |
| カラー無効化 | `--no-color` フラグ + `NO_COLOR` 環境変数（https://no-color.org）両方を尊重 |

## アーキテクチャ概要

```
fetch loop (tokio interval)
   │  reqwest → VtsStatus (serde)
   ▼
App state ──── derived metrics (RPS, p95, BW/s)
   │                  ▲
   │             prev snapshot
   ▼
ratatui render ─→ terminal
   ▲
crossterm events (key, resize, ctrl-c)
```

詳細は `docs/DESIGN.md` 参照。

## 依存クレート

- `ratatui` 0.29+ — TUI（Sparkline / Gauge / Table / BarChart 同梱）
- `crossterm` 0.29+ (`event-stream`) — ターミナルバックエンド + 非同期 `EventStream`
- `tokio` (`rt`, `macros`, `time`, `signal`) — 非同期ランタイム（シングルスレッド）
- `futures-util` — `EventStream` への `StreamExt::next()` 適用（`tokio-stream` ではなく `futures` 側を使う）
- `reqwest` (`rustls-tls`, `json`) — HTTP（OpenSSL 不使用でクロスコンパイル容易）。`--insecure` 用に `danger_accept_invalid_certs` を利用
- `serde` + `serde_json` — vts JSON デシリアライズ
- `clap` v4 (`derive`) — CLI パース
- `color-eyre` — エラーレポート
- `humansize` — バイト単位の整形
- dev: `pretty_assertions`, `insta`

## プロジェクト構成

```
vozltop/
├── Cargo.toml
├── CLAUDE.md            ← 本ファイル
├── README.md
├── docs/
│   ├── DESIGN.md
│   └── ROADMAP.md
├── src/
│   ├── main.rs
│   ├── cli.rs
│   ├── client.rs        VtsClient
│   ├── model.rs         vts JSON 型
│   ├── state/{mod,history,derived}.rs
│   ├── ui/{mod,header,table,detail,footer,help}.rs
│   ├── event.rs
│   └── theme.rs
└── tests/
    ├── fixtures/*.json
    ├── deserialize.rs
    └── derived.rs
```

## キー割り当て（v1）

| キー | 動作 |
|------|------|
| Tab / Shift+Tab | zone 種別切替（Server → Upstream → Cache） |
| ↑ ↓ / k j | 行カーソル移動 |
| PgUp / PgDn | ページ送り |
| Enter | 選択行の詳細オーバーレイ |
| Esc | 詳細 / フィルタ解除 |
| F1 | ヘルプ |
| F4 / `/` | フィルタ（zone 名 substring） |
| F5 | ソート方向反転 |
| 1-9 | ソート列指定（タブごとに列構成が異なる。詳細は `docs/DESIGN.md` 参照） |
| F10 / q / Ctrl-C | 終了 |

F-key を奪うターミナル（macOS Terminal.app 等）向けの letter エイリアス: `?` = F1, `q` = F10。

## CLI

```
vozltop <URL> [OPTIONS]

OPTIONS:
  -i, --interval <SECONDS>  リフレッシュ間隔 [default: 1.0]
  -u, --user <USER:PASS>    HTTP Basic 認証
  -H, --header <K: V>       追加ヘッダ（繰り返し可）
      --insecure            TLS 証明書検証を無効化
      --no-color            色を無効化（環境変数 NO_COLOR=1 でも同等）
```

`NO_COLOR` 環境変数がセットされている場合、`--no-color` 指定が無くても自動的にモノクロ描画にフォールバックする（https://no-color.org に準拠）。

## 開発フロー

```bash
cargo build                  # ビルド
cargo test                   # 全テスト（unit + insta snapshot）
cargo clippy --all-targets -- -D warnings
cargo run -- http://localhost:8080/status/format/json
```

E2E は Docker で nginx-vts を立てて手動確認:
```bash
docker run --rm -p 8080:80 -d xcgd/nginx-vts
ab -n 5000 -c 50 http://localhost:8080/    # トラフィック生成
cargo run -- http://localhost:8080/status/format/json --interval 0.5
```

## 実装順序（v1）

1. ドキュメント整備（このファイル / DESIGN.md / ROADMAP.md / README.md）
2. `Cargo.toml` + `.gitignore` + 空 `src/main.rs` で `cargo build` 通す（`rust-version = "1.74"` 明記）
3. **Docker で xcgd/nginx-vts を立て、実 response を取得して `tests/fixtures/` に保存**
   - histogram あり (`vhost_traffic_status_histogram_buckets` 設定) と なし の両パターン
   - 手書きせず、`curl http://localhost:8080/status/format/json` の生 JSON を commit する
4. `model.rs` + `tests/deserialize.rs`（両 fixture をパース、`requestBuckets` の数値型は実 JSON で確定）
5. `client.rs`（reqwest fetch、認証は後段）
6. `state/derived.rs` + テスト（histogram あり / なし両方の派生メトリクス）
7. `event.rs` + `main.rs` ループ統合
8. `ui/header.rs`
9. `ui/table.rs` + Tab 切替 + ソート / フィルタ
10. `ui/detail.rs` + `ui/help.rs` + `ui/footer.rs`
11. 認証フラグを `client.rs` に追加
12. README にスクリーンショット
13. 初回コミット & push

## Phase 2 候補

`docs/ROADMAP.md` 参照。
