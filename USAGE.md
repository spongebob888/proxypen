# proxypen — Usage

A small toolkit for probing SOCKS5 proxies (and the direct path) with:

- **HTTP protocol tests** over HTTP/1.1, HTTP/2, HTTP/3
- **TCP / UDP throughput benchmarks** with packet-loss, jitter, and one-way latency
- **Press / retest** — latency percentile (p50, p99) measurement at configurable concurrency
- **Local HTTP/1, HTTP/2, HTTP/3 test server** for benchmarking against a known target

The same `--proxy` / `--interface` flags drive both. Omit `--proxy` to test
directly. On a direct test you may bind the outgoing socket to a specific NIC
with `--interface <name|ip>`.

---

## Build

```sh
cargo build --release
# binary: ./target/release/proxypen
```

---

## Quick start

```sh
# 1. HTTP protocol probe through a SOCKS5 proxy (default: HTTP/1+2+3)
proxypen -p socks5://127.0.0.1:1080 -t https://www.cloudflare.com/

# 2. Same probe, but direct (no proxy)
proxypen -t https://www.cloudflare.com/

# 3. One-shot throughput benchmark — local server in the same process
proxypen benchmark --serve --duration 5

# 4. Two-host throughput benchmark
#    on the server box:
proxypen server --bind 0.0.0.0 --port 5555
#    on the client box:
proxypen benchmark --target server.example:5555 --mode udp --udp-bandwidth 10M,50M,200M

# 5. Start a local HTTP/1, HTTP/2, HTTP/3 test server
proxypen serve-http

# 6. Press-test HTTP/1 against the local server (100 requests, 10 concurrent)
proxypen press -t http://127.0.0.1:8080 -P http1 -c 10 -n 100

# 7. Press-test HTTP/3 through a SOCKS5 proxy
proxypen press -p socks5://127.0.0.1:1080 -t https://example.com -P http3 -c 20 -n 500 --insecure
```

---

## Subcommands

| Command                  | What it does                                                  |
|--------------------------|---------------------------------------------------------------|
| `proxypen ...` (flat)    | Same as `proxypen test ...` — backward-compatible default     |
| `proxypen test`          | HTTP/1, HTTP/2, HTTP/3 correctness + timing probe             |
| `proxypen benchmark`     | TCP/UDP throughput client                                     |
| `proxypen server`        | Standalone bench server, for two-host setups                  |
| `proxypen press`         | Latency retest: p50/p99 TTFB at given concurrency             |
| `proxypen serve-http`    | Local HTTP/1 + HTTP/2 + HTTP/3 test server (self-signed cert) |

---

## HTTP protocol test

```
proxypen [test] [-p PROXY] [-i IFACE] -t URL [-P PROTO] [-T SECS] [-r] [-v]
```

| Flag                 | Meaning                                                       |
|----------------------|---------------------------------------------------------------|
| `-p, --proxy URL`    | SOCKS5 proxy `socks5://[user:pass@]host:port`. Omit ⇒ direct. |
| `-i, --interface S`  | Direct mode only. Interface name (`en0`) or local IP.         |
| `-t, --target URL`   | `http[s]://host[:port]/path`                                  |
| `-P, --protocol P`   | `http1` \| `http2` \| `http3` \| `all` (default)              |
| `-T, --timeout SECS` | Per-test timeout. Default `30`.                               |
| `-r, --resolve`      | Resolve DNS locally with the system resolver (instead of letting the proxy do it). |
| `--dns-server ADDR`  | Use this DNS server (IP or IP:PORT, default port 53). The query is sent via the configured transport — see "DNS resolution" below. |
| `-v, --verbose`      | Debug logging.                                                |

Examples:

```sh
# All three protocols through a SOCKS5 proxy
proxypen -p socks5://127.0.0.1:1080 -t https://www.cloudflare.com/

# Just HTTP/3, direct, bound to interface en0
proxypen -i en0 -t https://www.cloudflare.com/ -P http3

# Direct via a specific source IP
proxypen -i 192.168.1.42 -t https://example.com/ -P http2

# With auth
proxypen -p socks5://alice:secret@proxy.example:1080 -t https://api.example/v1
```

Output (one line per protocol):

```
[HTTP/1.1] OK 200 (493ms) tcp:71ms tls:88ms ttfb:251ms size:1.4MB
```

| Field   | Source mode  | Meaning                                                  |
|---------|--------------|----------------------------------------------------------|
| `socks` | proxy        | Time to complete the SOCKS5 handshake                    |
| `tcp`   | direct       | Time to complete the direct TCP connect                  |
| `tls`   | TLS / QUIC   | TLS or QUIC handshake duration                           |
| `ttfb`  | both         | Time to first response byte                              |
| `size`  | both         | Response body size                                       |

HTTP/3 in direct mode has no `tcp`/`socks` field (UDP is connectionless).

---

## Benchmark (client)

```
proxypen benchmark [-p PROXY] [-i IFACE] [-t HOST:PORT] [--serve [--serve-port N]]
                   [-m MODE] [-d DIR] [-D SECS]
                   [--udp-bandwidth LIST] [--udp-size BYTES] [--tcp-chunk BYTES] [-v]
```

Transport (same semantics as `test`):

| Flag                 | Meaning                                                  |
|----------------------|----------------------------------------------------------|
| `-p, --proxy URL`    | Send the bench through this SOCKS5 proxy.                |
| `-i, --interface S`  | Direct mode only. Bind to interface name or local IP.    |

Targeting:

| Flag                       | Meaning                                              |
|----------------------------|------------------------------------------------------|
| `-t, --target HOST:PORT`   | Bench server address. Required unless `--serve`.     |
| `--serve`                  | Spin a server in the same process on `127.0.0.1`.    |
| `--serve-port N`           | Pin the in-process server to a specific port.        |

Test selection:

| Flag                       | Default | Meaning                                       |
|----------------------------|---------|-----------------------------------------------|
| `-m, --mode tcp\|udp\|both`| `both`  | Which protocol(s) to test                     |
| `-d, --direction up\|down\|both` | `both` | Upload, download, or both                |
| `-D, --duration SECS`      | `10`    | Per-test duration                             |
| `--udp-bandwidth LIST`     | `10M`   | Comma-separated SI rates (`K/M/G`, base 10)   |
| `--udp-size BYTES`         | `1200`  | UDP datagram size (incl. 16-byte header)      |
| `--tcp-chunk BYTES`        | `65536` | TCP read/write chunk                          |

For `--mode both --direction both` the run executes:
`TCP up`, `TCP down`, then for each `--udp-bandwidth` value: `UDP up`, `UDP down`.

Examples:

```sh
# One-shot, default plan (TCP+UDP, both directions, 10s, UDP @ 10M)
proxypen benchmark --serve

# UDP-only sweep through a SOCKS5 proxy
proxypen benchmark --proxy socks5://127.0.0.1:1080 --target server.example:5555 \
                   --mode udp --udp-bandwidth 1M,5M,10M,50M,100M --duration 5

# Compare proxy vs direct for the same host pair
proxypen benchmark --target server.example:5555 --mode udp --udp-bandwidth 50M
proxypen benchmark --target server.example:5555 --mode udp --udp-bandwidth 50M \
                   --proxy socks5://127.0.0.1:1080

# Only TCP, only download, 30s, larger chunk
proxypen benchmark --target server.example:5555 --mode tcp -d down -D 30 --tcp-chunk 262144

# Direct, force traffic out of en0
proxypen benchmark --serve --interface en0 --mode tcp
```

---

## Bench server (standalone)

```
proxypen server [-b BIND] [--port N] [-v]
```

| Flag                | Default     | Meaning                          |
|---------------------|-------------|----------------------------------|
| `-b, --bind IP`     | `0.0.0.0`   | Address to bind the listener on  |
| `--port N`          | `5555`      | TCP control port                 |
| `-v, --verbose`     |             | Debug logging                    |

The server is unaware of SOCKS5 — proxying happens entirely on the client. The
server accepts a TCP control connection per session and opens a fresh ephemeral
TCP/UDP data port per test, so a single server instance can serve many runs.

```sh
# Listen on all interfaces, default port
proxypen server

# Loopback only, fixed port
proxypen server --bind 127.0.0.1 --port 5555
```

---

## Press / retest (latency percentiles)

```
proxypen press -t URL -P PROTO [-p PROXY] [-i IFACE] [-c N] [-n N] [-T SECS] [--insecure] [-v]
```

| Flag                  | Meaning                                                       |
|-----------------------|---------------------------------------------------------------|
| `-t, --target URL`    | `http[s]://host[:port]/path`. **Required.**                   |
| `-P, --protocol P`    | `http1` \| `http2` \| `http3` (default: `http1`)               |
| `-p, --proxy URL`     | SOCKS5 proxy `socks5://[user:pass@]host:port`. Omit ⇒ direct. |
| `-i, --interface S`   | Direct mode only. Interface name (`en0`) or local IP.         |
| `-c, --concurrency N` | Number of concurrent connections. Default `10`.               |
| `-n, --num-requests N`| Total number of requests to send. Default `100`.              |
| `-T, --timeout SECS`  | Per-request timeout. Default `30`.                            |
| `--insecure`          | Skip TLS certificate verification (for self-signed servers).  |
| `-r, --resolve`       | Resolve DNS locally instead of letting the proxy do it.       |
| `--dns-server ADDR`   | Use this DNS server for resolution.                           |
| `-v, --verbose`       | Debug logging.                                                |

The press command opens `-c` concurrent connections and sends `-n` total
requests, measuring TTFB (Time To First Byte) and total request duration for
each one. Latency distributions (min, avg, p50, p90, p95, p99, max) are
computed from the successful responses.

Output:

```
=== Press Test Results ===
Protocol:        HTTP/1.1
Total requests:  100
Successful:      100
Failed:          0
Duration:        0.02s
Req/sec:         5203.47

--- TTFB (Time To First Byte) ---
  min:     0.31 ms
  avg:     0.68 ms
  p50:     0.58 ms
  p90:     0.95 ms
  p95:     1.21 ms
  p99:     1.52 ms
  max:     1.64 ms

--- Total Request Time ---
  min:     0.42 ms
  avg:     0.79 ms
  p50:     0.66 ms
  p90:     1.12 ms
  p95:     1.63 ms
  p99:     3.81 ms
  max:     4.12 ms
```

Examples:

```sh
# HTTP/1, 50 concurrent, 500 total requests, direct
proxypen press -t http://127.0.0.1:8080 -P http1 -c 50 -n 500

# HTTP/2, 20 concurrent, 200 requests through a SOCKS5 proxy (self-signed cert)
proxypen press -p socks5://127.0.0.1:1080 -t https://server:8443 -P http2 \
                -c 20 -n 200 --insecure

# HTTP/3, direct, bound to a specific interface
proxypen press -i en0 -t https://example.com -P http3 -c 5 -n 50

# HTTP/2 against a public server (no --insecure needed)
proxypen press -t https://www.cloudflare.com -P http2 -c 10 -n 100
```

---

## HTTP test server (local)

```
proxypen serve-http [-b BIND] [--http1-port N] [--http2-port N] [--http3-port N]
                    [--response-size BYTES]
```

| Flag                       | Default       | Meaning                                     |
|----------------------------|---------------|---------------------------------------------|
| `-b, --bind IP`            | `127.0.0.1`   | Address to bind all listeners on.           |
| `--http1-port N`           | `8080`        | Plain HTTP/1.1 port.                        |
| `--http2-port N`           | `8443`        | TLS HTTP/2 port (ALPN: h2, http/1.1).       |
| `--http3-port N`           | `8444`        | QUIC HTTP/3 port.                           |
| `--response-size BYTES`    | `128`         | Body size of every response (filled with x).|

Starts all three HTTP protocol servers on separate ports using a single
self-signed TLS certificate (CN: `proxypen-test.local`). Use `--insecure` on
the client side to skip verification when testing against it.

The server prints the exact press commands you need after startup:

```
=== HTTP Test Server ===
Certificate CN: proxypen-test.local

HTTP/1  (plain) → http://127.0.0.1:8080
HTTP/2  (TLS)   → https://127.0.0.1:8443
HTTP/3  (QUIC)  → https://127.0.0.1:8444

Press Ctrl+C to stop.

Test commands:
  proxypen press -t http://127.0.0.1:8080 -P http1 -c 10 -n 100
  proxypen press -t https://127.0.0.1:8443 -P http2 -c 10 -n 100 --insecure
  proxypen press -t https://127.0.0.1:8444 -P http3 -c 10 -n 100 --insecure
```

Examples:

```sh
# Default setup on localhost
proxypen serve-http

# Bind to all interfaces, custom ports
proxypen serve-http --bind 0.0.0.0 --http1-port 80 --http2-port 443 --http3-port 443

# Larger response body (1 KB)
proxypen serve-http --response-size 1024
```

---

## Reading benchmark output

TCP block:

```
== TCP upload ==
  sent: 4.92 GB    rate: 13.13 Gbit/s    duration: 3.00s
  recv (server side): 4.92 GB
```

UDP block:

```
== UDP upload @ 50.00 Mbit/s ==
  sent:    15624 pkt /   18.75 MB    rate: 50.00 Mbit/s
  recv:    15624 pkt /   18.75 MB    rate: 50.00 Mbit/s
  loss: 0.00%   ooo: 0   dup: 0   jitter: 0.00 ms
  latency min/avg/max: 0.00 ms / 0.00 ms / 1.87 ms    target: 50.00 Mbit/s
```

| Field      | Meaning                                                                |
|------------|------------------------------------------------------------------------|
| `sent`     | Total packets/bytes the sender pushed during the test                  |
| `recv`     | Total received by the receiver. Rate uses the sender's wall-clock.     |
| `loss`     | `(sent - recv) / sent` — packets the receiver never saw                |
| `ooo`      | Out-of-order arrivals (sequence number went backwards)                 |
| `dup`      | Duplicate sequence numbers seen on the receiver                        |
| `jitter`   | RFC 3550 inter-arrival jitter estimator                                |
| `latency`  | Per-packet receive_ts − send_ts. **Relative**, see caveat below.       |
| `target`   | Configured target bandwidth for this run                               |

### One-way latency caveat

The latency numbers are anchored at the *first packet* of each test. They are
honest measurements of variation around that baseline, but they are **not**
absolute one-way delay — there is no clock sync between client and server.
Use them to compare runs between the same host pair (e.g. proxy vs direct, or
two proxy implementations), not as a standalone metric.

---

## DNS resolution

By default, hostname resolution follows the transport:

- **Direct mode:** the system resolver (`/etc/resolv.conf` on Unix) is used.
- **SOCKS5 mode:** the hostname is sent to the proxy and the proxy resolves
  it. Pass `--resolve` to force the system resolver to do it locally, then
  send the IP through the proxy.

Pass `--dns-server <IP[:PORT]>` to do the lookup yourself against a chosen
server:

- `--proxy + --dns-server`  → the query is sent over SOCKS5 UDP ASSOCIATE
  (requires the proxy to support UDP relay).
- `--interface + --dns-server` → the query is sent on a UDP socket bound to
  that interface.
- `--dns-server` alone → plain UDP query on the default route.

Only A records are supported today; AAAA is not.

```sh
# Resolve via Cloudflare, then HTTP/1 to that IP directly
proxypen --dns-server 1.1.1.1 -t http://example.com/ -P http1

# Direct test on en0, with DNS also on en0
proxypen -i en0 --dns-server 8.8.8.8 -t https://example.com/ -P http2

# Through a SOCKS5 proxy, DNS via the proxy at a custom server
proxypen -p socks5://127.0.0.1:1080 --dns-server 9.9.9.9 -t https://example.com/

# Bench: resolve the bench server's hostname via your DNS, route via proxy
proxypen benchmark -p socks5://127.0.0.1:1080 --dns-server 1.1.1.1 \
                   --target bench.example.com:5555 --mode tcp
```

## Notes

- **Interface binding** uses `IP_BOUND_IF` / `IPV6_BOUND_IF` on macOS (no
  privileges) and `SO_BINDTODEVICE` on Linux (requires `CAP_NET_RAW` or root).
  Source-IP binding works without privileges on either OS.
- **High UDP rates**: the pacer batches sends within each scheduler tick so
  rates well into the hundreds of Mbit/s are accurate. Loopback throughput is
  limited by the kernel UDP buffers, not the pacer.
- **SOCKS5 UDP**: the bench reuses the same `UDP ASSOCIATE` machinery as the
  HTTP/3 probe — verify a proxy supports UDP relay before expecting UDP results.
