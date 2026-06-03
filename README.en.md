# vozltop

[![CI](https://github.com/astail/vozltop/actions/workflows/ci.yml/badge.svg)](https://github.com/astail/vozltop/actions/workflows/ci.yml)
[![Security audit](https://github.com/astail/vozltop/actions/workflows/audit.yml/badge.svg)](https://github.com/astail/vozltop/actions/workflows/audit.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

[日本語](README.md) | **English**

`htop`-like real-time TUI for [vozlt/nginx-module-vts](https://github.com/vozlt/nginx-module-vts).

## What is this?

nginx-module-vts publishes per-vhost / upstream / cache traffic statistics as JSON. `vozltop` is a TUI that lets you watch those stats `htop`-style — **a single binary that launches instantly, supports sort/filter out of the box, and works over ssh.**

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

- **Header title**: `● {status} · {host} · up {uptime}`. The status dot (`●`) color reflects the connection state (green = Running, yellow = Stale or Connecting, red = Disconnected). When Stale / Disconnected, the consecutive failure count `(N)` is appended.
- **Conn row**: absolute breakdown of nginx connection state (`active / reading / writing / waiting`). No gauge / bar — `worker_connections` is not exposed by VTS JSON, so there is nothing meaningful to normalize against.
- **RPS / IN / OUT rows**: the current value (RPS as `N/s`, IN/OUT in B/s..MB/s) followed by a `│` separator and a cumulative counter (`req` count / `rx` / `tx` bytes). The layout is bar-less: just the numbers stacked vertically.
- **Table**: a rounded box (plain in mono) titled `{TabName} · {visible} [/ {total}] [· filter "q"]`. The active sort column shows `↓` / `↑` in its header, and the selected row gets a `▶` (mono: `>`) cursor glyph. Numeric columns are right-aligned in both the header and the body.
- **Alerting**: when `--alert-p95-ms` is set and a row's p95 reaches the threshold, just that p95 cell is colored red-inverse. In multi-host mode an additional `⚠` badge appears next to the host name in the host tab bar.

Non-zero `5xx%` cells are colored red. When colors are disabled, all of the above degrade to text fallbacks like `[!]` / `(!)`.

## Install

### From crates.io (requires Rust toolchain)

```bash
cargo install vozltop
```

### Binary releases

Download the tarball for your OS / arch from [GitHub Releases](https://github.com/astail/vozltop/releases):

| OS | Arch | Archive name |
|----|------|--------------|
| Linux (musl, no glibc required) | x86_64 | `vozltop-<version>-x86_64-unknown-linux-musl.tar.gz` |
| Linux (musl) | aarch64 | `vozltop-<version>-aarch64-unknown-linux-musl.tar.gz` |
| macOS (Apple Silicon) | aarch64 | `vozltop-<version>-aarch64-apple-darwin.tar.gz` |

To verify, place the matching `SHA256SUMS` file alongside the archive and run `shasum -a 256 -c SHA256SUMS`.

### Debian / Ubuntu (.deb)

The filename follows the Debian convention `vozltop_<version>-1_<amd64|arm64>.deb`.
Download the `.deb` for your version / arch from the
[Releases page](https://github.com/astail/vozltop/releases/latest):

```bash
# e.g. v0.1.0 / amd64
curl -LO https://github.com/astail/vozltop/releases/download/v0.1.0/vozltop_0.1.0-1_amd64.deb
sudo dpkg -i vozltop_0.1.0-1_amd64.deb
```

### Fedora / RHEL (.rpm)

The filename follows the RPM convention `vozltop-<version>-1.<x86_64|aarch64>.rpm`.

```bash
# e.g. v0.1.0 / x86_64
curl -LO https://github.com/astail/vozltop/releases/download/v0.1.0/vozltop-0.1.0-1.x86_64.rpm
sudo rpm -i vozltop-0.1.0-1.x86_64.rpm
```

### Homebrew (macOS / Linux)

```bash
brew install astail/tap/vozltop
```

> The Homebrew tap is maintained separately at `astail/homebrew-tap`. The formula template lives in this repo at `packaging/homebrew/vozltop.rb.template`.

## Usage

Just point it at the status endpoint of an nginx instance with nginx-module-vts enabled:

```bash
vozltop http://localhost/status/format/json
```

Change the refresh interval:

```bash
vozltop http://localhost/status/format/json --interval 0.5
```

Remote + Basic auth:

```bash
vozltop https://nginx.example.com/status/format/json \
  --user admin:secret
```

Custom header (e.g. Bearer token):

```bash
vozltop https://nginx.example.com/status/format/json \
  --header 'Authorization: Bearer eyJ...'
```

p95 alert (paints the p95 cell of any row at/above the threshold red-inverse + bell):

```bash
vozltop http://localhost/status/format/json --alert-p95-ms 500
```

### Multi-host monitoring

You can watch multiple nginx-vts instances at once. Pass two or more URLs and vozltop starts in multi-host mode, with a single-line Host tab bar at the very top of the screen.

```
HOST  [web-prod-1]  web-prod-2  edge-tokyo  api-asia⚠
```

The active host is surrounded by `[...]`, and any host whose p95 alert is firing gets a `⚠` (mono: `(!)`) badge — so you can spot trouble on another host without switching to it.

```bash
vozltop http://web-prod-1/status/format/json \
        http://web-prod-2/status/format/json \
        http://edge-tokyo/status/format/json
```

Each host is fetched in parallel by its own task; thresholds like `alert_p95_ms` come from the shared CLI flags and apply to every host. Switch hosts with `]` (next) / `[` (prev), or Shift+L / Shift+H.

You can also use multiple `[hosts.*]` entries from a config file simultaneously:

```bash
vozltop @prod @staging @edge
```

When you supply multiple aliases, only the URLs from the config are applied — per-host options like `interval` / `user` are ignored and the global CLI values are used instead. With a single alias, the original behavior (config-driven flag fallback) still applies.

### TOML config + alias launch

Save hosts you use often to `~/.config/vozltop/config.toml` and invoke them as `vozltop @<alias>`.

```toml
# ~/.config/vozltop/config.toml (mode 0600 recommended)

[hosts.prod]
url = "https://nginx.prod.example.com/status/format/json"
user = "admin:secret"
interval = 0.5
alert_p95_ms = 500

[hosts.staging]
url = "https://nginx.staging.example.com/status/format/json"
```

```bash
vozltop @prod                       # uses url / user / interval / alert from config
vozltop @prod --interval 2.0        # CLI flags override config
vozltop --config ./custom.toml @x   # explicit config path
VOZLTOP_CONFIG=~/x.toml vozltop @x  # config path via env var
```

Precedence: **CLI flags > `[hosts.<alias>]` from config > built-in defaults.** When you pass a URL directly (no alias), config is ignored (for compatibility).

Config lookup order:

1. `--config <path>` (explicit)
2. `$VOZLTOP_CONFIG` environment variable
3. `directories::ProjectDirs` (Linux: `~/.config/vozltop/config.toml` / macOS: `~/Library/Application Support/vozltop/config.toml` / Windows: `%APPDATA%\vozltop\config.toml`)

If none exist, vozltop runs without a config (= v1 compatible).

Passwords are stored as plain text in the config. Keyring integration is being considered for Phase 2. Run `chmod 0600` on the file so other users can't read it.

### Keeping secrets out of argv

On shared machines (jump hosts, kubernetes pods, etc.) other users can see your `argv` via `ps`, so passing `--user user:pass` or `--header 'Authorization: Bearer ...'` directly leaks the password / token. vozltop provides these fallbacks to avoid that:

| Use case | How |
|----------|-----|
| Basic auth password | `VOZLTOP_PASSWORD` env var (recommended) |
| Basic auth password (from a script) | `--user user:-` + stdin pipe |
| Whole header | `--header @path/to/file` (file contains one `K: V` line) |

```bash
# env var (recommended)
VOZLTOP_PASSWORD=secret vozltop https://nginx.example.com/status/format/json \
  --user admin:placeholder

# stdin (pipe from a password manager)
pass show vozltop | vozltop https://nginx.example.com/status/format/json --user admin:-

# file (mode 0600 recommended)
echo 'Authorization: Bearer eyJ...' > ~/.vozltop-auth
chmod 600 ~/.vozltop-auth
vozltop https://nginx.example.com/status/format/json --header @~/.vozltop-auth
```

When `VOZLTOP_PASSWORD` is set, the `argv_pass` part of `--user user:argv_pass` is **always overridden** (the argv value can be a dummy). If a password / Bearer token is still present in argv, vozltop prints a warning to stderr once at startup.

### TLS verification

By default, reqwest's **rustls-tls-native-roots** backend loads the OS trust store (macOS Keychain / system CA on Linux, etc.). If your corporate CA is registered with the OS, HTTPS works without any extra configuration.

Allow self-signed / expired certificates (use only on trusted networks):

```bash
vozltop https://nginx.example.com/status/format/json --insecure
```

> ⚠️ `--insecure` **completely disables TLS certificate verification.** Use only on trusted networks. vozltop prints a yellow warning to stderr once at startup. If you just want to use a corporate CA, register it in the OS trust store rather than passing `--insecure`.

Disable colors (the `--no-color` flag and the `NO_COLOR` env var both follow [no-color.org](https://no-color.org)):

```bash
vozltop http://localhost/status/format/json --no-color
NO_COLOR=1 vozltop http://localhost/status/format/json
```

### Zone tabs

Switch with `Tab` / `Shift+Tab`. The order is `Server → Upstream → Cache → Filter → Server …`.

| Tab | Columns | Source |
|-----|---------|--------|
| Server Zones | ZONE / RPS / 2xx% / 4xx% / 5xx% / p95 / IN/s / OUT/s | `serverZones` |
| Upstream Servers | same + STATE (up/backup/down) | `upstreamZones` expanded to 1 row per server (`ZONE` column = `group/host:port`) |
| Cache Zones | ZONE / HIT% / MISS / EXPIRED / STALE / USED / IN/s / OUT/s | `cacheZones` |
| Filter Zones | ZONE (`group/key`) / RPS / 2xx% / 4xx% / 5xx% / p95 / IN/s / OUT/s | `filterZones` |

Press `Enter` to open the detail overlay for the selected zone (p50 / p95 / p99, the per-bucket histogram for the last tick, and the breakdown of response counts).

### Key bindings

| Key | Action |
|-----|--------|
| Tab / Shift+Tab | Switch zone type (Server / Upstream / Cache / Filter) |
| ↑ ↓ / k j | Move row cursor |
| PgUp / PgDn | Page up / down |
| Enter | Open detail overlay / close it if already open |
| Esc | Close detail / clear filter / close help |
| F1 / `?` | Help |
| F4 / `/` | Filter zones by substring (case-insensitive) |
| F5 | Reverse sort direction |
| 1-9 | Select sort column (per-tab column list) |
| `[` / `]` (Shift+H / Shift+L) | Switch host (multi-host only) |
| F10 / q / Ctrl-C | Quit |

macOS Terminal.app intercepts F1-F4, so the letter aliases `?` (= F1), `/` (= F4), and `q` (= F10) are provided as fallbacks.

## nginx configuration example

```nginx
http {
    vhost_traffic_status_zone;
    vhost_traffic_status_filter_by_host on;
    # Optional: ms-resolution histogram (recommended for p95/p99)
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

If `vhost_traffic_status_histogram_buckets` is not set, the table's p95 column falls back to the average response time (prefixed with `~Nms`).

## Build

```bash
git clone https://github.com/astail/vozltop
cd vozltop
cargo build --release
./target/release/vozltop --help
```

## End-to-end test

```bash
# Run nginx-vts in Docker
docker run --rm -p 8080:80 -d --name nginx-vts xcgd/nginx-vts

# Generate traffic in another terminal
ab -n 10000 -c 50 http://localhost:8080/

# Watch with vozltop
cargo run -- http://localhost:8080/status/format/json --interval 0.5
```

## Roadmap

See `docs/ROADMAP.md`. Multi-host monitoring, TOML config files, and the `filterZones` view have already shipped. Remaining Phase 2 candidates include resets via `/status/control`, upstream group-aggregation rows, and keyring integration.

## Security

If you find a vulnerability, please don't open a public issue — report it via [GitHub Private Vulnerability Reporting](https://github.com/astail/vozltop/security/advisories/new). See [SECURITY.md](SECURITY.md) for details.

## License

[MIT License](LICENSE) © 2026 astail
