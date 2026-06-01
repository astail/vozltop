# Histogram バケット動作確認 (お試し用)

詳細オーバーレイ (Enter) の histogram BarChart `<=5  <=10  <=50  <=100  <=500  <=1000  <=5000  >10000` が PDF として正しく描画されるかを目視で確認するための、ローカル仕掛けの手順。

- 何を見たいか: 各バケットに異なる req/s を流したとき、vozltop のヒストグラムが各バケットに **分散** して立つこと (issue #134 で「全部同じ高さに並ぶ」CDF 表示を PDF 化した変更の検証)
- 想定環境: macOS + Docker Desktop (`host.docker.internal` を使う)
- 前提: 既に [tests/fixtures/setup/](../tests/fixtures/setup/) ベースの nginx-vts コンテナが立っていて、ホストの `/tmp/vozltop-nginx/` を `/etc/nginx/conf.d` に bind mount している (read-only でも host 側に書けば反映される)

## 仕掛けの構成

```
ab × 6 + curl loop ──► nginx-vts (slow.example.com vhost)
                          │  /fast       → return 200 (nginx 内完結 = <=5ms)
                          └─ /d/<ms>     → proxy_pass http://host.docker.internal:9100
                                              │
                                              ▼
                                       Python ThreadingHTTPServer
                                       path の数字を ms とみなして sleep
```

## 1. ホスト側に Python slow server を立てる

`/<ms>` を叩くと `<ms>` ミリ秒 sleep してから 200 を返すだけのサーバ。

```bash
python3 -c '
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import time

class H(BaseHTTPRequestHandler):
    def do_GET(self):
        tok = self.path.lstrip("/").split("?")[0].split("/")[0]
        ms = int(tok) if tok.isdigit() else 0
        time.sleep(ms / 1000.0)
        self.send_response(200); self.end_headers()
        self.wfile.write(b"ok\n")
    def log_message(self, *a, **k): pass

ThreadingHTTPServer(("127.0.0.1", 9100), H).serve_forever()
' &
```

スモークテスト:

```bash
curl -w "ms=%{time_total}\n" -o /dev/null -s http://127.0.0.1:9100/50
# → ms=0.05x
```

## 2. nginx に slow.example.com vhost を追加

`proxy_pass` の URL に変数 (`$ms`) を入れると runtime DNS が必要で、`resolver` 未設定の場合 502 になる。ここでは `rewrite` で path を書き換えてから **固定 upstream** に飛ばす (起動時 DNS lookup で済む)。

```bash
cat > /tmp/vozltop-nginx/slow.example.com.conf <<'NGINX'
server {
    listen 80;
    server_name slow.example.com;

    # nginx 内完結 (<=5ms バケット狙い)
    location = /fast {
        return 200 "fast\n";
    }

    # /d/<ms> → host:9100/<ms> (Python slow server が <ms> ミリ秒 sleep)
    location ~ ^/d/\d+$ {
        rewrite ^/d/(\d+)$ /$1 break;
        proxy_pass http://host.docker.internal:9100;
        proxy_http_version 1.1;
        proxy_set_header Host $host;
    }
}
NGINX
docker exec vozltop-vts nginx -s reload
```

スモークテスト:

```bash
for ms in 3 30 300 1500; do
  curl -s -H "Host: slow.example.com" -o /dev/null \
       -w "/d/%5s → %{http_code} %{time_total}s\n" \
       --max-time 20 http://localhost:8080/d/$ms
done
# /d/    3 → 200 0.027s
# /d/   30 → 200 0.037s
# /d/  300 → 200 0.308s
# /d/ 1500 → 200 1.515s
```

Python + proxy のオーバヘッドが **約 25ms** 乗るので、`/d/3` でも実測 ~27ms (<=50 バケット行き) になることに注意。

## 3. ab fleet で各バケットを狙って流す

並列度 `-c` を遅延に比例させて RPS を揃え (各 ~5-30 req/s)、`-n` を巨大値にしてループせず連続させる。`-t` は ab 内部で `-n 50000` cap がかかって早期終了するため使わない。

```bash
H='Host: slow.example.com'
U='http://localhost:8080'

# <=5  : nginx 内 return 200 (curl loop で RPS を意図的に抑える。ab だと c=1 でも数百 req/s 出てしまい他バケットが見えなくなる)
( while sleep 0.2; do curl -s -H "$H" $U/fast -o /dev/null; done ) &

# <=50  ── <=5000 : Python sleep upstream 経由
ab -n 100000000 -c 1  -H "$H" $U/d/30    >/dev/null 2>&1 &   # <=50
ab -n 100000000 -c 1  -H "$H" $U/d/80    >/dev/null 2>&1 &   # <=100
ab -n 100000000 -c 2  -H "$H" $U/d/300   >/dev/null 2>&1 &   # <=500
ab -n 100000000 -c 4  -H "$H" $U/d/800   >/dev/null 2>&1 &   # <=1000
ab -n 100000000 -c 15 -H "$H" $U/d/3000  >/dev/null 2>&1 &   # <=5000

# >10000 : 12 秒 sleep。c を大きくしないと観測される RPS が出ない
ab -n 100000000 -c 50 -H "$H" $U/d/12000 >/dev/null 2>&1 &
```

## 4. vozltop で確認

```bash
cargo run --release -- http://localhost:8080/status/format/json --interval 0.5
```

Tab で Server zone に居る状態で `slow.example.com` 行に矢印で移動 → **Enter**。各バケットに分散した histogram が立っていれば PDF 化が動作している。

35 秒間の差分は概ねこうなる (実測例):

```
label       PDF  分布
<=5         160  █████████████████████
<=10          0
<=50        296  ████████████████████████████████████████
<=100       128  █████████████████
<=500       105  ██████████████
<=1000      122  ████████████████
<=5000      154  ████████████████████
>10000        3
```

## 既知の制約

- **`<=10` バケットは埋まらない**: nginx 内で 5〜10ms の固定遅延を作る手段が必要 (`ngx_http_echo_module` の `echo_sleep` か lua の `ngx.sleep`) だが、arquivei/nginx-vts イメージにも tests/fixtures/setup/ のビルドにもこれらは含まれていない。`/d/1` を叩いても proxy オーバヘッドで <=50 バケットに流れる。観測できないだけで PDF 化の正しさには影響しない。
- **`>10000` バケットは観測値が少なめ**: `/d/12000` は 1 req あたり 12 秒かかるため、c=50 でも完了は遅い。長時間流せば自然に積もる。

## 止め方

```bash
pkill ab
pkill -f slow_server                 # python サーバ
pkill -f 'curl.*slow.example.com'    # /fast ループ
rm /tmp/vozltop-nginx/slow.example.com.conf
docker exec vozltop-vts nginx -s reload
```

## 参考

- 関連 issue: [#134 — histogram bar が CDF のまま並ぶ (PDF 化したい)](https://github.com/astail/vozltop/issues/134)
- 関連 PR: [#135 — fix(ui/detail): histogram バーを PDF 化する](https://github.com/astail/vozltop/pull/135)
