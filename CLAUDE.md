# CLAUDE.md — vozltop

このファイルは Claude Code が本リポジトリで作業するときに毎回参照するプロジェクトコンテキストです。

## プロジェクト概要

**vozltop** は [vozlt/nginx-module-vts](https://github.com/vozlt/nginx-module-vts) が公開する nginx トラフィック統計 JSON を、`htop` のようなインタラクティブ TUI でリアルタイム可視化する CLI ツールです。

- 言語: **Rust**（stable のみ。MSRV 宣言は持たない — issue #79 参照）
- 配布: 単一バイナリ（`cargo install vozltop` / GitHub Releases）
- ターゲット: linux x86_64/arm64, macOS arm64
- ライセンス: 未確定（v1 リリース前に MIT を予定）

## コーディング指針（Karpathy 原則）

LLM コーディングで起こりがちな失敗（思い込みで進める / 過剰実装 / 関係ない箇所まで書き換える）を抑えるためのガイドライン。出典: [multica-ai/andrej-karpathy-skills](https://github.com/multica-ai/andrej-karpathy-skills)（[Karpathy の元投稿](https://x.com/karpathy/status/2015883857489522876)）。trivial なタスクでは状況に応じて判断する。

### 1. 実装する前に考える

**思い込まない。混乱を隠さない。トレードオフを表に出す。**

- 仮定は明示する。不確かなら聞く。
- 解釈が複数ありえるなら全部出す。黙って一つに決めない。
- もっと単純な方法があるなら言う。必要ならユーザーに反論する。
- 何かが不明なら止まる。何が不明かを名指しして質問する。

### 2. シンプル優先

**問題を解く最小コード。投機的なものは書かない。**

- 頼まれていない機能は足さない。
- 1 箇所でしか使わないコードを抽象化しない。
- 頼まれていない「柔軟性」「設定可能性」を入れない。
- 起こりえないケースのエラーハンドリングを書かない。
- 200 行書いて 50 行で済むなら書き直す。

セルフチェック: 「シニアエンジニアがこれを見て『過剰』と言わないか？」言うなら簡素化する。

### 3. 外科的な変更

**触る必要があるところだけ触る。自分が散らかした分だけ片付ける。**

既存コードを編集するとき:
- 周辺コード / コメント / フォーマットを「ついでに改善」しない。
- 壊れていないものをリファクタしない。
- 自分の好みと違っても既存スタイルに合わせる。
- 関係ない dead code に気づいたら言及だけする。削除しない。

自分の変更で orphan が生まれたとき:
- 自分の変更で未使用になった import / 変数 / 関数は消す。
- 元から残っていた dead code は頼まれない限り消さない。

セルフチェック: 変更した全行が、ユーザーの依頼に直接ひも付くか？

### 4. ゴール駆動の実行

**成功条件を定義する。検証できるまでループする。**

タスクを検証可能なゴールに置き換える:
- 「バリデーションを追加」→「不正入力のテストを書き、それを通す」
- 「バグ修正」→「バグを再現するテストを書き、それを通す」
- 「X をリファクタ」→「前後でテストが通ることを確認する」

複数ステップなら短い計画を出す:
```
1. [手順] → 検証: [チェック]
2. [手順] → 検証: [チェック]
3. [手順] → 検証: [チェック]
```

成功条件が強いほど自走できる。「動くようにして」だけだと毎回確認が必要になる。

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
| 設定ファイル | v1 は CLI 引数のみ。Phase 2 で `~/.config/vozltop/config.toml` + `@alias` 起動を追加 (#46)。keyring は別 issue |
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

- `ratatui` **0.29.x** (= 0.29) — TUI（Sparkline / Gauge / Table / BarChart 同梱）。v1 期間中は 0.30 へ追従しない (詳細: [docs/notes/ratatui-030-evaluation.md](docs/notes/ratatui-030-evaluation.md))
- `crossterm` 0.29+ (`event-stream`) — ターミナルバックエンド + 非同期 `EventStream`
- `tokio` (`rt`, `macros`, `time`, `signal`) — 非同期ランタイム（シングルスレッド）
- `futures-util` — `EventStream` への `StreamExt::next()` 適用（`tokio-stream` ではなく `futures` 側を使う）
- `reqwest` (`rustls-tls-native-roots`, `json`, `gzip`) — HTTP（OpenSSL 不使用でクロスコンパイル容易）。**既定で OS の信頼ストアを使用** (社内 CA 等の追加設定不要)。`--insecure` 用に `danger_accept_invalid_certs` を利用 (詳細は issue #39 / SECURITY.md)
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
  -u, --user <USER:PASS>    HTTP Basic 認証 (password が `-` の場合 stdin から読み取り)
  -H, --header <K: V>       追加ヘッダ（繰り返し可。`@path/to/file` でファイル読込）
      --insecure            TLS 証明書検証を無効化
      --no-color            色を無効化（環境変数 NO_COLOR=1 でも同等）
      --alert-p95-ms <MS>   p95 が MS ミリ秒以上の行をハイライト + ベル

ENV:
  VOZLTOP_PASSWORD          設定時は `--user` の password を上書き（argv に secrets を残さないため）
  NO_COLOR                  非空でセットされていれば色を無効化（https://no-color.org）
```

`NO_COLOR` 環境変数がセットされている場合、`--no-color` 指定が無くても自動的にモノクロ描画にフォールバックする（https://no-color.org に準拠）。

argv に password / Bearer トークンが残っていると起動時に stderr に黄色で警告が 1 度出る (issue #40)。共有マシンでは `VOZLTOP_PASSWORD` / `--user user:-` / `--header @file` のいずれかを使う。

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
2. `Cargo.toml` + `.gitignore` + 空 `src/main.rs` で `cargo build` 通す
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
