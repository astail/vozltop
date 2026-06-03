# vozltop

[![CI](https://github.com/astail/vozltop/actions/workflows/ci.yml/badge.svg)](https://github.com/astail/vozltop/actions/workflows/ci.yml)
[![Security audit](https://github.com/astail/vozltop/actions/workflows/audit.yml/badge.svg)](https://github.com/astail/vozltop/actions/workflows/audit.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

**日本語** | [English](README.en.md)

`htop`-like real-time TUI for [vozlt/nginx-module-vts](https://github.com/vozlt/nginx-module-vts).

## なに？

nginx-module-vts は nginx の vhost / upstream / cache 単位のトラフィック統計を JSON で公開してくれる。`vozltop` はそれを `htop` のように **1 バイナリで起動・即ソート / フィルタ可能・ssh 越しに動く** TUI で眺めるためのツール。

```
╭ ● api.prod · up 3d 14h ──────────────────────────────────────────────────────╮
│ Conn   active 42  reading 3  writing 5  waiting 34                           │
│┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄│
│ RPS  1284/s      │  req  1.58 M                                              │
│ IN   12.4 MB/s   │  rx   1.50 GB                                             │
│ OUT  84.0 MB/s   │  tx   5.20 GB                                             │
╰──────────────────────────────────────────────────────────────────────────────╯
╭ Server Zones · 2 ────────────────────────────────────────────────────────────╮
│  ZONE               RPS↓   2xx%   4xx%   5xx%      p95      IN/s     OUT/s   │
│▶ api.example.com     842 100.0%   0.0%   0.0%     38ms  3.0 MB/s 24.0 MB/s   │
│  www.example.com     321 100.0%   0.0%   0.0%     12ms  5.0 MB/s 41.0 MB/s   │
╰──────────────────────────────────────────────────────────────────────────────╯
F1Help F4Filter F5Sort F10Quit  Tab:Zone Enter:Detail
```

- **ヘッダタイトル**: `● {status} · {host} · up {uptime}`。ステータスドット (`●`) の色で接続状態を示す (緑=Running / 黄=Stale or Connecting / 赤=Disconnected)。Stale / Disconnected のときは連続失敗回数 `(N)` も付く
- **Conn 行**: nginx 接続状況の絶対値ブレイクダウン (`active / reading / writing / waiting`)。worker_connections は VTS JSON に含まれないので gauge / bar は出さない
- **RPS / IN / OUT 行**: 現在値 (RPS は `N/s`、IN / OUT は B/s〜MB/s 等) + `│` 区切り + 累計 (`req` 件数 / `rx` / `tx` バイト数)。値そのものを縦に並べる素朴なレイアウト (bar は持たない)
- **テーブル**: rounded box (mono は plain) の中に `Tab名 · 表示件数 [/ 総件数] [· filter "q"]` のタイトル。ソート中の列見出しに `↓` / `↑`、選択カーソル行に `▶` (mono: `>`) が付く。数値列はヘッダ・値とも右寄せで揃う
- **アラート**: `--alert-p95-ms` が指定されていて p95 がしきい値以上のとき、その p95 セルだけ赤反転で着色される。multi-host 時は host タブの host 名にも `⚠` バッジが出る

`5xx%` が非ゼロのセルは赤系で着色される。色を切ったときは `[!]` / `(!)` 等のテキストフォールバックに変わる。

## インストール

### crates.io から (Rust toolchain が必要)

```bash
cargo install vozltop
```

### バイナリ release

[GitHub Releases](https://github.com/astail/vozltop/releases) から OS / アーキテクチャ別の tarball をダウンロード:

| OS | アーキテクチャ | アーカイブ名 |
|----|---------------|-------------|
| Linux (musl, glibc 不要) | x86_64 | `vozltop-<version>-x86_64-unknown-linux-musl.tar.gz` |
| Linux (musl) | aarch64 | `vozltop-<version>-aarch64-unknown-linux-musl.tar.gz` |
| macOS (Apple Silicon) | aarch64 | `vozltop-<version>-aarch64-apple-darwin.tar.gz` |

検証は `SHA256SUMS` ファイルを同じディレクトリに置いて `shasum -a 256 -c SHA256SUMS` で行えます。

### Debian / Ubuntu (.deb)

ファイル名は `vozltop_<version>-1_<amd64|arm64>.deb` 形式 (Debian convention)。
[Releases ページ](https://github.com/astail/vozltop/releases/latest) から該当
バージョン / アーキテクチャの `.deb` をダウンロード:

```bash
# 例: v0.1.0 / amd64
curl -LO https://github.com/astail/vozltop/releases/download/v0.1.0/vozltop_0.1.0-1_amd64.deb
sudo dpkg -i vozltop_0.1.0-1_amd64.deb
```

### Fedora / RHEL (.rpm)

ファイル名は `vozltop-<version>-1.<x86_64|aarch64>.rpm` 形式 (RPM convention)。

```bash
# 例: v0.1.0 / x86_64
curl -LO https://github.com/astail/vozltop/releases/download/v0.1.0/vozltop-0.1.0-1.x86_64.rpm
sudo rpm -i vozltop-0.1.0-1.x86_64.rpm
```

### Homebrew (macOS / Linux)

```bash
brew install astail/tap/vozltop
```

> Homebrew tap は `astail/homebrew-tap` で別管理されています。formula テンプレートは本リポジトリの `packaging/homebrew/vozltop.rb.template` を参照してください。

## 使い方

nginx-module-vts を組み込んだ nginx の status エンドポイントを指定するだけ:

```bash
vozltop http://localhost/status/format/json
```

リフレッシュ間隔を変える:

```bash
vozltop http://localhost/status/format/json --interval 0.5
```

リモート + Basic 認証:

```bash
vozltop https://nginx.example.com/status/format/json \
  --user admin:secret
```

任意ヘッダ（Bearer トークンなど）:

```bash
vozltop https://nginx.example.com/status/format/json \
  --header 'Authorization: Bearer eyJ...'
```

p95 アラート (しきい値以上の zone の p95 セルを赤反転 + ベル):

```bash
vozltop http://localhost/status/format/json --alert-p95-ms 500
```

### Multi-host 監視

複数の nginx-vts インスタンスを同時に監視できます。URL を 2 つ以上渡すと multi-host モードで起動し、画面最上段に 1 行の Host タブバーが出ます。

```
HOST  [web-prod-1]  web-prod-2  edge-tokyo  api-asia⚠
```

active host は `[...]` で囲まれ、p95 などのアラートが立っている host には `⚠` (mono: `(!)`) が付くため、他 host のヘルスもタブを切り替えずに気付けます。

```bash
vozltop http://web-prod-1/status/format/json \
        http://web-prod-2/status/format/json \
        http://edge-tokyo/status/format/json
```

各 host は独立した fetch task で並列に取得され、`alert_p95_ms` 等のしきい値は全 host 共通の CLI フラグから決まります。host 切替は `]` (次) / `[` (前) または Shift+L / Shift+H。

config の `[hosts.*]` セクションを複数同時に使うことも可能です:

```bash
vozltop @prod @staging @edge
```

複数 alias のときは URL の置換のみ行い、`interval` / `user` 等の per-host config は適用されません (グローバル CLI 値を使用)。単一 alias のときの挙動 (config 由来のフラグ補完) は従来通りです。

### TOML 設定ファイル + alias 起動

繰り返し使うホストは `~/.config/vozltop/config.toml` に登録して `vozltop @<alias>` で呼び出せます。

```toml
# ~/.config/vozltop/config.toml (mode 0600 推奨)

[hosts.prod]
url = "https://nginx.prod.example.com/status/format/json"
user = "admin:secret"
interval = 0.5
alert_p95_ms = 500

[hosts.staging]
url = "https://nginx.staging.example.com/status/format/json"
```

```bash
vozltop @prod                       # config の url / user / interval / alert を使用
vozltop @prod --interval 2.0        # CLI フラグは config を上書き
vozltop --config ./custom.toml @x   # config path を明示
VOZLTOP_CONFIG=~/x.toml vozltop @x  # 環境変数で path 指定
```

設定の優先度: **CLI フラグ > config の `[hosts.<alias>]` > 組み込み既定**。alias を使わずに URL を直指定する場合、config は無視されます (互換性維持)。

config 探索順:

1. `--config <path>` (明示指定)
2. `$VOZLTOP_CONFIG` 環境変数
3. `directories::ProjectDirs` (Linux: `~/.config/vozltop/config.toml` / macOS: `~/Library/Application Support/vozltop/config.toml` / Windows: `%APPDATA%\vozltop\config.toml`)

いずれも存在しない場合は config 無しで動作 (= v1 互換)。

password は config に平文で記載します。keyring 連携は Phase 2 で検討予定。`chmod 0600` で他ユーザから読めないようにすることを推奨します。

### Secrets を argv に露出しない

共有マシン (jump-host / kubernetes pod など) では `ps` で他ユーザに argv が見えるため、`--user user:pass` や `--header 'Authorization: Bearer ...'` を直接渡すと password / token が漏れます。本ツールはこれを回避するために以下のフォールバックを提供します:

| 用途 | 方法 |
|------|------|
| Basic 認証の password | `VOZLTOP_PASSWORD` 環境変数 (推奨) |
| Basic 認証の password (script 経由) | `--user user:-` + stdin パイプ |
| 任意ヘッダ全体 | `--header @path/to/file` (ファイル内に `K: V` 形式で 1 行記述) |

```bash
# 環境変数 (推奨)
VOZLTOP_PASSWORD=secret vozltop https://nginx.example.com/status/format/json \
  --user admin:placeholder

# stdin (パスワードマネージャから流す)
pass show vozltop | vozltop https://nginx.example.com/status/format/json --user admin:-

# ファイル (mode 0600 推奨)
echo 'Authorization: Bearer eyJ...' > ~/.vozltop-auth
chmod 600 ~/.vozltop-auth
vozltop https://nginx.example.com/status/format/json --header @~/.vozltop-auth
```

`VOZLTOP_PASSWORD` 環境変数を設定すると `--user user:argv_pass` の `argv_pass` は **常に上書き** されます (argv に書く値はダミーで OK)。argv に password / Bearer トークンが残っているのを検知すると起動時に stderr に警告を 1 度出します。

### TLS 検証

既定では reqwest の **rustls-tls-native-roots** バックエンドで OS の信頼ストア (macOS Keychain / Linux のシステム CA 等) を読み込みます。社内 CA を OS に登録していれば追加設定なしで HTTPS が通ります。

自己署名・期限切れ証明書を許容 (信頼できるネットワークでのみ使用):

```bash
vozltop https://nginx.example.com/status/format/json --insecure
```

> ⚠️ `--insecure` は **TLS 証明書検証を完全に無効化** します。Use only on trusted networks. 起動時に stderr に黄色で警告が 1 度出ます。社内 CA を使いたいだけなら `--insecure` ではなく OS の信頼ストアへの CA 登録を推奨します。

色を無効化（`--no-color` フラグまたは `NO_COLOR` 環境変数。両方とも [no-color.org](https://no-color.org) に準拠）:

```bash
vozltop http://localhost/status/format/json --no-color
NO_COLOR=1 vozltop http://localhost/status/format/json
```

### Zone タブ

`Tab` / `Shift+Tab` で切り替え。順は `Server → Upstream → Cache → Filter → Server …`。

| タブ | 列構成 | 表示元 |
|------|--------|--------|
| Server Zones | ZONE / RPS / 2xx% / 4xx% / 5xx% / p95 / IN/s / OUT/s | `serverZones` |
| Upstream Servers | 上記 + STATE (up/backup/down) | `upstreamZones` を 1 server = 1 行に展開 (`ZONE` 列は `group/host:port`) |
| Cache Zones | ZONE / HIT% / MISS / EXPIRED / STALE / USED / IN/s / OUT/s | `cacheZones` |
| Filter Zones | ZONE (`group/key`) / RPS / 2xx% / 4xx% / 5xx% / p95 / IN/s / OUT/s | `filterZones` |

`Enter` で選択 zone の詳細オーバーレイ (p50 / p95 / p99 + 1 tick ぶんの bucket 別ヒストグラム + responses 内訳) が開く。

### キー割り当て

| キー | 動作 |
|------|------|
| Tab / Shift+Tab | zone 種別切替 (Server / Upstream / Cache / Filter) |
| ↑ ↓ / k j | 行カーソル移動 |
| PgUp / PgDn | ページ送り |
| Enter | 詳細オーバーレイを開く / 表示中は閉じる |
| Esc | 詳細 / フィルタ / ヘルプを閉じる |
| F1 / `?` | ヘルプ |
| F4 / `/` | zone 名フィルタ (大文字小文字無視の substring 一致) |
| F5 | ソート方向反転 |
| 1-9 | ソート列指定 (タブごとに列構成が異なる) |
| `[` / `]` (Shift+H / Shift+L) | host 切替 (multi-host のみ) |
| F10 / q / Ctrl-C | 終了 |

macOS Terminal.app は F1-F4 を OS 側で奪うため、`?` (= F1) / `/` (= F4) / `q` (= F10) の letter alias を用意している。

## nginx 側設定例

```nginx
http {
    vhost_traffic_status_zone;
    vhost_traffic_status_filter_by_host on;
    # 任意: ms 単位のヒストグラム（p95/p99 を出すために推奨）
    vhost_traffic_status_histogram_buckets 0.005 0.01 0.05 0.1 0.5 1 5;

    server {
        listen 80;
        server_name _;

        location /status {
            vhost_traffic_status_display;
            vhost_traffic_status_display_format json;
            allow 127.0.0.1;
            deny all;
        }
    }
}
```

`vhost_traffic_status_histogram_buckets` を設定しない場合、テーブルの p95 は平均応答時間で代替表示されます（`~Nms` プレフィックス）。

## ビルド

```bash
git clone https://github.com/astail/vozltop
cd vozltop
cargo build --release
./target/release/vozltop --help
```

## 動作確認 (E2E)

```bash
# nginx-vts を Docker で起動
docker run --rm -p 8080:80 -d --name nginx-vts xcgd/nginx-vts

# 別ターミナルでトラフィック生成
ab -n 10000 -c 50 http://localhost:8080/

# vozltop で監視
cargo run -- http://localhost:8080/status/format/json --interval 0.5
```

## ロードマップ

`docs/ROADMAP.md` 参照。マルチホスト監視 / TOML 設定ファイル / `filterZones` ビューは既に出荷済み。残りの Phase 2 候補としては `/status/control` 経由のリセット、Upstream の group 集約行、keyring 連携等。

## セキュリティ

脆弱性を発見した場合は public な issue ではなく、[GitHub Private Vulnerability Reporting](https://github.com/astail/vozltop/security/advisories/new) からご連絡ください。詳細は [SECURITY.md](SECURITY.md) を参照してください。

## ライセンス

[MIT License](LICENSE) © 2026 astail
