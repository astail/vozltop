# vozltop

[![CI](https://github.com/astail/vozltop/actions/workflows/ci.yml/badge.svg)](https://github.com/astail/vozltop/actions/workflows/ci.yml)
[![Security audit](https://github.com/astail/vozltop/actions/workflows/audit.yml/badge.svg)](https://github.com/astail/vozltop/actions/workflows/audit.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

`htop`-like real-time TUI for [vozlt/nginx-module-vts](https://github.com/vozlt/nginx-module-vts).

> ⚠️ **Status: pre-alpha.** Specification fixed, implementation in progress. See [docs/DESIGN.md](docs/DESIGN.md) and [CLAUDE.md](CLAUDE.md) for current state.

## なに？

nginx-module-vts は nginx の vhost / upstream / cache 単位のトラフィック統計を JSON で公開してくれる。`vozltop` はそれを `htop` のように **1 バイナリで起動・即ソート / フィルタ可能・ssh 越しに動く** TUI で眺めるためのツール。

```
┌─ vozltop ─────────────────────────────────────────────────┐
│ Conn  active 142  reading 3  writing 12  waiting 127      │
│ RPS   ████████████░░░░░░░░  1,284   5xx ▏  0.3%            │
│ BW in ▇▇▇▇▇▇▂  12.4 MB/s    out ▇▇▇▇▇▇▇▇▇  84 MB/s        │
├────────────────────────────────────────────────────────────┤
│ [Server] Upstream  Cache                                  │
├────────────────────────────────────────────────────────────┤
│ ZONE             RPS    2xx   4xx  5xx  p95   IN/s  OUT/s │
│ api.example.com  842   99.1% 0.7% 0.2% 38ms  3MB  24MB    │
│ www.example.com  321   99.8% 0.1% 0.1% 12ms  5MB  41MB    │
├────────────────────────────────────────────────────────────┤
│ F1Help F4Filter F5Sort F10Quit  Tab:NextZone Enter:Detail │
└────────────────────────────────────────────────────────────┘
```

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

```bash
# x86_64 / arm64 のうち適切なものをダウンロード
curl -LO https://github.com/astail/vozltop/releases/latest/download/vozltop_amd64.deb
sudo dpkg -i vozltop_amd64.deb
```

### Fedora / RHEL (.rpm)

```bash
curl -LO https://github.com/astail/vozltop/releases/latest/download/vozltop-x86_64.rpm
sudo rpm -i vozltop-x86_64.rpm
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

### キー割り当て

| キー | 動作 |
|------|------|
| Tab / Shift+Tab | zone 種別切替 |
| ↑ ↓ / k j | 行カーソル移動 |
| Enter | 詳細オーバーレイ |
| Esc | 詳細 / フィルタ解除 |
| F1 | ヘルプ |
| F4 / `/` | zone 名フィルタ |
| F5 | ソート方向反転 |
| 1-9 | ソート列指定 |
| F10 / q / Ctrl-C | 終了 |

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

`docs/ROADMAP.md` 参照。マルチホスト監視、`/status/control` 経由のリセット、TOML 設定ファイル等を Phase 2 で予定。

## セキュリティ

脆弱性を発見した場合は public な issue ではなく、[GitHub Private Vulnerability Reporting](https://github.com/astail/vozltop/security/advisories/new) または `kiyomillefeuille@gmail.com` 宛にご連絡ください。詳細は [SECURITY.md](SECURITY.md) を参照してください。

## ライセンス

[MIT License](LICENSE) © 2026 astail
