# vozltop ロードマップ

## v1 (MVP)

`CLAUDE.md` と `docs/DESIGN.md` を参照。

## Phase 2

### 機能拡張

- **`/status/control` の reset / delete サポート**
  - F9 で選択中 zone のカウンタリセット
  - 二段確認モーダル（zone 名を再入力）
  - `--allow-control` フラグで明示的に有効化（デフォルト無効）

- **マルチホスト監視**
  - 複数 URL を引数で受ける: `vozltop host1.example.com/... host2.example.com/...`
  - Tab に Host も加わる: `[host1] host2  Server  Upstream  Cache`
  - 各ホストごとに独立したスナップショット履歴

- **`filterZones` ビュー**
  - `vhost_traffic_status_filter_by_set_key $geoip_country_code` 等で設定されたカスタム集計
  - Tab に Filter を追加、フィルタ名→キー名→値の 3 階層ナビ

- **TOML 設定ファイル**
  - `~/.config/vozltop/config.toml` でホスト/エイリアス/認証/閾値プリセット
  - `vozltop @prod` のようなエイリアス起動

- **Zone 単位の長期履歴 + トレンド表示**
  - 詳細ビューに 1h / 1d ウィンドウのスパークライン
  - 現在は集計ヘッダのみ履歴を持っている

- **アラート閾値**
  - `--alert-5xx-pct 1` `--alert-p95-ms 500` で行をハイライト + ベル
  - TOML 設定と統合

### インフラ

- **GitHub Actions release workflow**
  - tag push で `linux x86_64 musl` / `linux arm64 musl` / `macos arm64` バイナリを Release に添付
  - SHA256 と minisign 署名

- **Homebrew tap**
  - `brew install astail/tap/vozltop`

- **Linux パッケージ**
  - `.deb` / `.rpm` を release workflow から自動生成

### 監視データソース拡張

- **Prometheus pull モード**
  - `--source prometheus` で nginx-vts の Prometheus エンドポイントを直接読む
  - histogram バケツのフォーマットは Prom 側に揃っているので算出ロジック共通化が必要

- **OpenTelemetry collector 連携**
  - スナップショットを OTLP で投げて Grafana 等に貯められるオプション

## アイデア（未確定）

- `htop` の F2 Setup 相当: 表示カラムのカスタマイズ
- Vim ライクモード (`:`, `/`, gg/G)
- 配色テーマ切替（light/dark/solarized）
- TUI 上での `tail -f access.log` 風相関ビュー
