# vozltop 設計書

## 背景・動機

[vozlt/nginx-module-vts](https://github.com/vozlt/nginx-module-vts) は nginx に組み込んでホスト / アップストリーム / キャッシュ単位のトラフィック統計を JSON / HTML / Prometheus 形式で公開できる nginx モジュール。情報は十分にあるが、現状は以下のいずれかで眺めるしかない:

- HTML ダッシュボード — ブラウザが必要、リアルタイム性が弱い
- `watch curl ... | jq` — 読みづらい、並び替え/フィルタができない
- Prometheus + Grafana — 立ち上げに労力がかかる、ssh 越し運用に向かない

**vozltop** は `htop` の体験 — 1 バイナリで起動して即座にソート / フィルタできる端末 TUI — を nginx-vts のデータに当てる。

## 設計判断と理由

### レイアウト: htop 風 1 テーブル

- 上部にサマリ（接続数ゲージ、総 RPS スパークライン、5xx 率、BW スパークライン）
- 下部に並び替え可能テーブル 1 つ
- Tab で zone 種別（Server / Upstream / Cache）切替

**理由:** タブ式や分割ペインは情報密度を犠牲にする。htop が示したように、サマリ + 1 テーブルが最も認知負荷が低く、ssh 越しの狭い端末でも機能する。

### Zone スコープ: serverZones / upstreamZones / cacheZones（v1）

- `filterZones` は柔軟だが運用設定が必要で、ユーザーが揃っていない可能性が高い → Phase 2
- `connections` と `sharedZones` はヘッダのみで使う

### Upstream 行の粒度: 1 server = 1 行

`upstreamZones` は `HashMap<String, Vec<UpstreamServer>>` の構造（group → server 配列）。v1 では **server 1 台 = 1 行**として描画し、`ZONE` 列に `group/host:port` を表示する。group 単位の集約行は単一 server の障害を隠してしまうため Phase 2 に倒す。

### 認証: デフォルト無し + オプションフラグ

nginx-vts の `/status` は通常 `allow 127.0.0.1; deny all;` で IP 制限される運用が多く、ローカル実行なら認証不要で叩ける。リモート監視時に必要となるパターンを `--user` / `--header` / `--insecure` でカバーする。

### Read-only（v1）

`/status/control` の `reset` / `delete` は本番カウンタを破壊し得る危険な操作。htop の kill/nice と違い、誤爆の影響範囲が大きく、確認 UX を雑にすると事故るため Phase 2 に倒す。

### レイテンシ: p95 をテーブル、p50/p95/p99 を詳細ビュー

`requestMsec`（単純平均）はテール検知に使えない。`requestBuckets` から線形補間で p95 を出すのが nginx-vts 上で実現できる最良の近似。histogram 未設定 zone は `~Nms` プレフィックスで平均表示にフォールバック。

## アーキテクチャ

```
                    ┌────────────────────────┐
                    │ tokio runtime (1 thread) │
                    └────────────────────────┘
                              │
        ┌─────────────────────┼─────────────────────┐
        ▼                     ▼                     ▼
  interval tick          crossterm                ctrl-c
        │                EventStream                 │
        ▼                     │                      │
  VtsClient::fetch()           │                      │
        │                     │                      │
        ▼                     ▼                      ▼
                        AppEvent enum
                              │
                              ▼
                    App::handle(event)
                              │
                              ▼
                       terminal.draw(ui::render)
```

イベント駆動シングルスレッド。`tokio::select!` で 3 ソースをマージし、`AppEvent` で正規化して `App` に流す。

## データモデル (model.rs)

```rust
#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct VtsStatus {
    pub host_name: String,
    pub nginx_version: String,
    pub module_version: String,
    pub load_msec: u64,
    pub now_msec: u64,
    pub connections: Connections,
    #[serde(default)]
    pub server_zones: HashMap<String, ServerZone>,
    #[serde(default)]
    pub upstream_zones: HashMap<String, Vec<UpstreamServer>>,
    #[serde(default)]
    pub cache_zones: HashMap<String, CacheZone>,
    #[serde(default)]
    pub shared_zones: SharedZones,
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct Connections {
    pub active: u64, pub reading: u64, pub writing: u64,
    pub waiting: u64, pub accepted: u64, pub handled: u64,
    pub requests: u64,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ServerZone {
    pub request_counter: u64,
    pub in_bytes: u64,
    pub out_bytes: u64,
    pub responses: Responses,
    pub request_msec_counter: u64,
    pub request_msec: u64,
    #[serde(default)]
    pub request_buckets: Option<Buckets>,
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct Responses {
    #[serde(rename = "1xx")] pub r1xx: u64,
    #[serde(rename = "2xx")] pub r2xx: u64,
    #[serde(rename = "3xx")] pub r3xx: u64,
    #[serde(rename = "4xx")] pub r4xx: u64,
    #[serde(rename = "5xx")] pub r5xx: u64,
    #[serde(default)] pub miss: u64,
    #[serde(default)] pub bypass: u64,
    #[serde(default)] pub expired: u64,
    #[serde(default)] pub stale: u64,
    #[serde(default)] pub updating: u64,
    #[serde(default)] pub revalidated: u64,
    #[serde(default)] pub hit: u64,
    #[serde(default)] pub scarce: u64,
}

#[derive(Deserialize, Debug, Clone)]
pub struct Buckets {
    // vts は整数ミリ秒で出力する（nginx 設定では秒の小数で書くが、JSON では ms 整数に変換される）。
    // 実 response の fixture を tests/fixtures に置き、deserialize テストで型を裏取りすること。
    pub msecs: Vec<u64>,
    pub counters: Vec<u64>,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct UpstreamServer {
    pub server: String,            // "10.0.0.1:8080" 形式
    pub request_counter: u64,
    pub in_bytes: u64,
    pub out_bytes: u64,
    pub responses: Responses,
    pub request_msec_counter: u64,
    pub request_msec: u64,
    #[serde(default)] pub request_buckets: Option<Buckets>,
    pub response_msec_counter: u64,
    pub response_msec: u64,
    #[serde(default)] pub response_buckets: Option<Buckets>,
    #[serde(default)] pub weight: u64,
    #[serde(default)] pub max_fails: u64,
    #[serde(default)] pub fail_timeout: u64,
    #[serde(default)] pub backup: bool,
    #[serde(default)] pub down: bool,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CacheZone {
    pub max_size: u64,
    pub used_size: u64,
    pub in_bytes: u64,
    pub out_bytes: u64,
    pub responses: Responses,      // hit/miss/bypass/expired/stale/updating/revalidated/scarce
}
```

v1 のレイテンシ表示には Upstream の `request_buckets`（クライアントから見たトータル）を使う。`response_buckets`（upstream 応答のみ）は Phase 2 で詳細ビューに追加する。

## 派生メトリクス (state/derived.rs)

### RPS / BW

```
rps = (req_now - req_prev) / (now_msec - prev_msec) * 1000
bw_in_per_sec  = (in_bytes_now  - in_bytes_prev)  / dt
bw_out_per_sec = (out_bytes_now - out_bytes_prev) / dt
```

`nowMsec` はサーバ提供値を使う（クライアント時計の skew 回避）。

### パーセンタイル

`requestBuckets.counters` は累積カウンタなので、2 スナップショット間の差分を取って "この区間でのバケツ別件数" を得る。

```text
delta[i] = counters_now[i] - counters_prev[i]    // 区間 i の発生件数
total = sum(delta)
target = total * p (p=0.5, 0.95, 0.99)
cum[i] = Σ delta[0..=i]
i = 最初に cum[i] >= target となるバケツ
p_value = msecs[i-1] + (msecs[i]-msecs[i-1]) * (target - cum[i-1]) / delta[i]
```

**エッジケース:**
- `total == 0`（区間内にリクエスト無し）→ 直前値を保持、初回は `—` 表示
- `delta[i] == 0`（補間不能）→ `msecs[i-1]` を採用
- `target` が最終バケツの cum を超える（histogram 上限を超えたテール）→ `>Nms`（N = 最終 `msecs`）プレフィックスで上限値を表示
- 累積カウンタが nginx 再起動でリセット（now < prev）→ 当該スナップショットの派生メトリクスは無効化、prev を更新するのみ

**histogram 未設定 zone:**
`request_msec`（区間平均）を `~Nms` プレフィックス付きで表示。p95 列でソートする際は、histogram あり zone を先にソートし、`~Nms` 表示の zone はその後に独立してソートする（混在ソートは値の意味が違って誤解を生むため）。

### キャッシュ hit 率

```
hit% = hit / (hit + miss + bypass + expired + stale + updating + revalidated + scarce) * 100
```

分母 0 のときは「—」。

## UI レイアウト

```
┌─ vozltop ─────────────────────────────────────────────────┐
│ Conn  active 142  reading 3  writing 12  waiting 127      │  ← header.rs
│ RPS   ████████████░░░░░░░░                          1,284  │     (4 row)
│ in    ▇▇▇▇▇▇▂                                  12.4 MB/s  │
│ out   ▇▇▇▇▇▇▇▇▇                                  84 MB/s  │
├────────────────────────────────────────────────────────────┤
│ [Server] Upstream  Cache                                  │  ← tab bar
├────────────────────────────────────────────────────────────┤
│ ZONE             RPS    2xx   4xx  5xx  p95   IN/s  OUT/s │  ← table.rs
│ api.example.com  842   99.1% 0.7% 0.2% 38ms  3MB  24MB    │
│ www.example.com  321   99.8% 0.1% 0.1% 12ms  5MB  41MB    │
│ admin.internal    11  100.0% 0.0% 0.0%  9ms  0.1MB 0.1MB  │
├────────────────────────────────────────────────────────────┤
│ F1Help F4Filter F5Sort F10Quit  Tab:NextZone Enter:Detail │  ← footer.rs
└────────────────────────────────────────────────────────────┘
```

- 接続数 Gauge の max は JSON から取得不能（`worker_connections` が無い）ため、**起動後に観測された `connections.active` の rolling-max を最大値として auto-scale** する。
- RPS / BW の Sparkline は最新 120 スナップショット（1s 間隔で 2 分相当）を保持。
- F2 Setup と F3 は v1 未実装。footer には掲載しない。

Enter で中央 50% に詳細オーバーレイ:

```
        ┌─ api.example.com ──────────────────────┐
        │ p50  18ms   p95  38ms   p99  142ms     │
        │                                        │
        │     ▁▂▃▆█▇▅▃▂▁                         │  ← BarChart (buckets)
        │  5  10  25  50  100  250  500  1000 ms │     ← 軸ラベルは
        │                                        │       requestBuckets.msecs から
        │                                        │       実行時に生成（ハードコードしない）
        │ 1xx 0   2xx 822  3xx 12  4xx 6  5xx 2  │
        │                                        │
        │ Esc / Enter to close                   │
        └────────────────────────────────────────┘
```

## イベントループ (event.rs + main.rs)

```rust
enum AppEvent {
    Tick(VtsStatus),
    Key(KeyEvent),
    Resize(u16, u16),
    Quit,
}

use futures_util::StreamExt;   // EventStream に .next() を生やす
use crossterm::event::EventStream;  // crossterm = { version = "0.29", features = ["event-stream"] }

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    // setup terminal
    let args = Args::parse();
    let client = VtsClient::new(&args)?;
    let mut interval = tokio::time::interval(Duration::from_secs_f64(args.interval));
    let mut events = EventStream::new();
    let mut ctrl_c = Box::pin(tokio::signal::ctrl_c());
    let mut app = App::default();

    loop {
        let ev: AppEvent = tokio::select! {
            _ = interval.tick() => match client.fetch().await {
                Ok(s) => AppEvent::Tick(s),
                Err(e) => { app.set_error(e); continue; }
            },
            Some(Ok(crossterm_evt)) = events.next() => map_event(crossterm_evt),
            _ = &mut ctrl_c => AppEvent::Quit,
        };
        if app.handle(ev).is_quit() { break; }
        terminal.draw(|f| ui::render(f, &app))?;
    }
}
```

## ソート列マッピング

タブごとに表示列が異なるため、数字キー（1-9）でソート列を指定する際のマッピングをタブ別に固定する。タブ切替で同じキーが別カラムを指す副作用は許容する（footer に現在のソート列名を常時表示してユーザーに認知させる）。

### Server タブ

| Key | 列 | 説明 |
|-----|----|------|
| 1 | ZONE | zone 名 alphabetical |
| 2 | RPS | requests/sec (差分) |
| 3 | 2xx% | 成功率 |
| 4 | 4xx% | クライアントエラー率 |
| 5 | 5xx% | サーバエラー率 |
| 6 | p95 | レイテンシ (histogram なし zone は別群) |
| 7 | IN/s | inbound bytes/sec |
| 8 | OUT/s | outbound bytes/sec |

### Upstream タブ

| Key | 列 | 説明 |
|-----|----|------|
| 1 | ZONE | `group/host:port` alphabetical |
| 2 | RPS | requests/sec |
| 3 | 2xx% | 成功率 |
| 4 | 4xx% | クライアントエラー率 |
| 5 | 5xx% | サーバエラー率 |
| 6 | p95 | request 全体レイテンシ |
| 7 | IN/s | inbound bytes/sec |
| 8 | OUT/s | outbound bytes/sec |
| 9 | STATE | down / backup / up（up を先頭） |

### Cache タブ

| Key | 列 | 説明 |
|-----|----|------|
| 1 | ZONE | cache zone 名 |
| 2 | HIT% | hit / (hit+miss+bypass+expired+stale+updating+revalidated+scarce) |
| 3 | MISS | miss カウント |
| 4 | EXPIRED | expired カウント |
| 5 | STALE | stale カウント |
| 6 | USED | used_size / max_size |
| 7 | IN/s | inbound bytes/sec |
| 8 | OUT/s | outbound bytes/sec |

デフォルトソートはどのタブでも `RPS` 降順（Cache タブは `HIT%` 降順）。F5 で方向反転。

## エラー / 状態遷移

```
[起動]
  │
  ▼
[Connecting...]  ←  最初の fetch 完了まで全画面に表示
  │
  ├─ 成功 ──→ [Running]  ← 通常描画
  │             │
  │             ├─ fetch error (transient) ──→ [Stale data + banner]
  │             │     └─ 次回 tick 成功で [Running] へ復帰
  │             │
  │             └─ fetch error (5 連続失敗) ──→ [Disconnected]
  │                   └─ 復帰で [Running] へ
  │
  └─ 失敗 (起動直後) ──→ [Connecting...] のままリトライ継続
```

| 状態 | 描画 | 派生メトリクス |
|------|------|----------------|
| Connecting | 中央に `Connecting to <URL>...` | 計算しない |
| Running | 通常 | prev snapshot との差分から算出 |
| Stale data | 通常描画 + 上部に黄色 banner `Last update Xs ago — fetch failed: ...` | 直前値を保持、RPS は 0 として扱わず `—` |
| Disconnected | 通常描画 + 赤 banner `Disconnected (N failures)` | 同上 |

- `nowMsec` がサーバ再起動で巻き戻った場合は当該スナップショットの派生計算を捨てて prev を更新するのみ。
- Content-Type が `application/json` 以外 / JSON パース失敗は transient エラーとして扱う（URL が間違っているケースを救うため、5 連続失敗で `Disconnected` 表示）。

## CLI (cli.rs)

`clap::Parser` derive。`docs/../CLAUDE.md` の CLI セクション参照。

## テスト戦略

1. **deserialize.rs** — fixture を `VtsStatus` にパースし、`server_zones["api.example.com"].request_counter` などを assert
   - fixture は **Docker で xcgd/nginx-vts を起動して `curl` で取得した実 JSON** を commit する（手書き禁止）
   - `histogram_buckets` あり / なしの 2 種類を最低限揃える
2. **derived.rs** — 2 スナップショット（initial / after_traffic）を作って RPS / p95 / BW を期待値と比較
   - histogram あり / なし、最終バケツ超過、counter リセット（nginx 再起動）、`delta = 0` の各エッジケース
3. **ui スナップショット** — `ratatui::backend::TestBackend` で 80×24 にレンダリング、`insta` で差分検出
   - `Connecting` / `Running` / `Stale data` / `Disconnected` の 4 状態を各タブで撮影
4. **clippy** — `-D warnings` で全 lint をエラー扱い

## 非機能要件

- 起動から最初の描画まで < 200ms（ratatui のレンダラはほぼ瞬時、reqwest の初回 fetch がボトルネック）
- メモリ使用 < 20MB（ヒストリ 120 スナップショット）
- 1s 間隔リフレッシュで CPU 使用率 < 1%

## 既知の制約

- vts モジュール側で histogram バケツが設定されていない場合、p50/p95/p99 は計算不可（平均値にフォールバック）
- 計測されたレイテンシが histogram の最終バケツ上限を超えた場合、`>Nms` 表示で打ち切り（実値は不明）
- nowMsec はサーバ時計ベースなので、再起動直後は前回スナップショットが無く RPS 計算がスキップされる
- 接続数 Gauge の最大値は `worker_connections × worker_processes` の正確値ではなく、起動後の rolling-max を使う近似
- crossterm の EventStream は Windows でも動くが、F-key の割り当ては Linux/macOS ターミナル前提（macOS Terminal.app では F1-F4 が OS にトラップされるため iTerm2/Alacritty/Kitty 推奨。`?` / `/` / `q` の letter エイリアスでカバー）
