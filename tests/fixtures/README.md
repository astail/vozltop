# VTS JSON フィクスチャ

`vozlt/nginx-module-vts` (以下 VTS) が公開する JSON は `tests/deserialize.rs` /
`tests/derived.rs` などで「実 response を decode できること」を保証するために
本ディレクトリへコミットしている。

**手書きしない**。再取得が必要になったときは `setup/` の Dockerfile と
nginx.conf を使って下記手順を踏み、ホスト名や IP に個人情報・社内情報が
含まれていないことを確認してから commit する。

## 同梱フィクスチャ

| ファイル | 用途 | 構成 |
|----------|------|------|
| `initial.json` | 派生メトリクス (RPS / BW/s 等) 計算用の 1 スナップショット目 | `nginx-no-histogram.conf` で起動直後に取得 |
| `after_traffic.json` | 派生メトリクス計算用の 2 スナップショット目 | `initial.json` と同一コンテナで `ab -n 5000 -c 50` 後に取得 |
| `no_histogram.json` | `vhost_traffic_status_histogram_buckets` 未設定時のスキーマ確認用 | 別コンテナで取得 (`requestBuckets.msecs` / `.counters` が空配列) |
| `with_histogram.json` | histogram 設定時のスキーマ確認用 | `nginx-with-histogram.conf` (`0.005 0.01 0.05 0.1 0.5 1 5` を設定) で取得 |

すべて `serverZones` / `upstreamZones` / `cacheZones` の 3 種を含む。
`serverZones` の `api.example.test` / `web.example.test` は RFC 6761 で予約された
`.test` TLD を使った合成名で、実在ホストではない。

## 取得手順

### 1. フィクスチャ用イメージをビルド

```bash
cd tests/fixtures/setup
docker build --platform linux/arm64 -t vozltop-nginx-vts:fixtures .
# x86_64 環境なら --platform linux/amd64 でも可
```

このイメージは `nginx:1.27.3-alpine` をベースに
[`vozlt/nginx-module-vts` v0.2.5](https://github.com/vozlt/nginx-module-vts/releases/tag/v0.2.5)
を `--add-dynamic-module` でビルドした VTS モジュール (.so) を組み込んだもの。
vozltop バイナリには同梱されず、フィクスチャ再取得時にのみ使う。

### 2. `initial.json` と `after_traffic.json` の取得

```bash
docker run -d --rm \
  -p 8080:80 -p 8081:8081 \
  -v "$PWD/nginx-no-histogram.conf:/etc/nginx/nginx.conf:ro" \
  --name nginx-vts vozltop-nginx-vts:fixtures

# プロキシ経由のキャッシュ ZONE を出現させるためのプライム
for h in api.example.test web.example.test; do
  curl -s -o /dev/null -H "Host: $h" "http://localhost:8080/"
  curl -s -o /dev/null -H "Host: $h" "http://localhost:8080/"
done

# 1 つ目のスナップショット (= initial)
curl -sS http://localhost:8081/status/format/json > ../initial.json

# トラフィック生成
ab -n 5000 -c 50 -H "Host: api.example.test" http://localhost:8080/
ab -n 2500 -c 25 -H "Host: web.example.test" http://localhost:8080/
# 4xx / 5xx を混ぜたい場合は適宜 curl で 404 を叩く

# 2 つ目のスナップショット (= after_traffic)
curl -sS http://localhost:8081/status/format/json > ../after_traffic.json

docker rm -f nginx-vts
```

### 3. `no_histogram.json` の取得

`after_traffic.json` と同一コンテナで撮ると derived metrics 用ペアと
ペイロードが重複するので、別コンテナを立て直して撮る。

```bash
docker run -d --rm \
  -p 8080:80 -p 8081:8081 \
  -v "$PWD/nginx-no-histogram.conf:/etc/nginx/nginx.conf:ro" \
  --name nginx-vts vozltop-nginx-vts:fixtures

for h in api.example.test web.example.test; do
  curl -s -o /dev/null -H "Host: $h" "http://localhost:8080/"
done
ab -n 5000 -c 50 -H "Host: api.example.test" http://localhost:8080/
ab -n 2500 -c 25 -H "Host: web.example.test" http://localhost:8080/

curl -sS http://localhost:8081/status/format/json > ../no_histogram.json
docker rm -f nginx-vts
```

### 4. `with_histogram.json` の取得

```bash
docker run -d --rm \
  -p 8080:80 -p 8081:8081 \
  -v "$PWD/nginx-with-histogram.conf:/etc/nginx/nginx.conf:ro" \
  --name nginx-vts vozltop-nginx-vts:fixtures

for h in api.example.test web.example.test; do
  curl -s -o /dev/null -H "Host: $h" "http://localhost:8080/"
done
ab -n 5000 -c 50 -H "Host: api.example.test" http://localhost:8080/
ab -n 3000 -c 30 -H "Host: web.example.test" http://localhost:8080/

curl -sS http://localhost:8081/status/format/json > ../with_histogram.json
docker rm -f nginx-vts
```

## 個人情報チェック

commit 前に以下を満たすことを確認すること。

- `hostName` フィールドは Docker のランダムコンテナ ID (例 `54a48c223301`) であり
  作業 PC のホスト名や社内サーバ名を含まない
- `serverZones` キーは `.test` TLD のみ
- `upstreamZones[].server` は `127.0.0.1:<port>` のみ
- 上記いずれも個人や内部システムを特定する情報を含まない

```bash
jq -r '{hostName, sv: (.serverZones|keys), up: ([.upstreamZones[][].server]|unique)}' ../*.json
```

## 受け入れ条件 (再取得時のセルフチェック)

- [ ] 4 ファイル全てを取得し、`jq empty *.json` でパース可能であること
- [ ] `serverZones` / `upstreamZones` / `cacheZones` の 3 種を各ファイルが含むこと
- [ ] `no_histogram.json` の `requestBuckets.msecs` / `.counters` が空配列であること
- [ ] `with_histogram.json` の `requestBuckets.msecs` / `.counters` が同じ長さで
      `msecs` の要素が `nginx-with-histogram.conf` で指定したバケット秒×1000 (ms) と
      一致すること
- [ ] `cargo test --test deserialize` が通ること
