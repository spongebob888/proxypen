# proxypen — Usage

A small toolkit for probing SOCKS5 proxies (and the direct path) with:

- **HTTP protocol tests** over HTTP/1.1, HTTP/2, HTTP/3
- **TCP / UDP throughput benchmarks** with packet-loss, jitter, and one-way latency

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
```

---

## Subcommands

| Command                | What it does                                                  |
|------------------------|---------------------------------------------------------------|
| `proxypen ...` (flat)  | Same as `proxypen test ...` — backward-compatible default     |
| `proxypen test`        | HTTP/1, HTTP/2, HTTP/3 correctness + timing probe             |
| `proxypen benchmark`   | TCP/UDP throughput client                                     |
| `proxypen server`      | Standalone bench server, for two-host setups                  |

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
| `-r, --resolve`      | Resolve DNS locally (instead of letting the proxy do it).     |
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

## Notes

- **Interface binding** uses `IP_BOUND_IF` / `IPV6_BOUND_IF` on macOS (no
  privileges) and `SO_BINDTODEVICE` on Linux (requires `CAP_NET_RAW` or root).
  Source-IP binding works without privileges on either OS.
- **High UDP rates**: the pacer batches sends within each scheduler tick so
  rates well into the hundreds of Mbit/s are accurate. Loopback throughput is
  limited by the kernel UDP buffers, not the pacer.
- **SOCKS5 UDP**: the bench reuses the same `UDP ASSOCIATE` machinery as the
  HTTP/3 probe — verify a proxy supports UDP relay before expecting UDP results.
