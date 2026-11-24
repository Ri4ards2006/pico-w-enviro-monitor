# Architecture: power management & data transmission

## Tasks

| task | role | static RAM |
|---|---|---|
| `main` | sensor init, WiFi join/reconnect, sample → render → upload loop | 2.6 KiB |
| `cyw43_task` | CYW43439 gSPI runner (PIO0 SM0 + DMA_CH0) | 4.3 KiB |
| `net_task` | embassy-net / smoltcp runner (DHCP, DNS, TCP) | 136 B |

Single thread-mode executor, all futures statically allocated, no heap.

## Cycle (default `INTERVAL_SECS = 300`)

```
 t=0 ─ Ticker tick
   │  link down? ──yes──▶ join(SSID) with 2→4→…→60 s back-off, wait for DHCP
   │
   ├─ BME280: write ctrl_meas (forced) → poll status (≤10×10 ms) → read 8 B burst
   │          → compensate (T, p, RH) → plausibility gate (range + NaN)
   │
   ├─ render body (JSON | ThingSpeak form)            heapless::String<256>
   │
   ├─ upload, hard ceiling 20 s (with_timeout):
   │     DNS A query (skipped for IPv4 literals)
   │     TCP connect (10 s socket timeout)
   │     write head + body, flush
   │     read until "HTTP/1.x NNN" parsed → 2xx = ok, else Status(n)
   │     close
   │
   └─ ticker.next().await ── executor idle → CPU in WFE until next tick
```

`Ticker` keeps a **fixed cadence**: upload time does not drift the period.
Every failure (sensor, DNS, connect, I/O, timeout, non-2xx) is logged and the
cycle is skipped — the node never blocks or panics on network trouble.

## Power management

| component | state between samples | mechanism |
|---|---|---|
| RP2040 cores | WFE sleep, clocks running | embassy executor sleeps when no task is ready; woken by timer alarm / IRQs |
| CYW43439 | associated, **PM `Aggressive`** | radio sleeps between DTIM beacons; strongest *officially supported* mode in `cyw43 0.7` (`SuperSave` is marked unsupported) |
| BME280 | **sleep mode** (~0.1 µA) | forced mode: one conversion per `read`, sensor returns to sleep on its own |
| I2C / TCP | idle | socket is closed after every upload |

### Why not deep sleep / DORMANT

The RP2040 has a DORMANT state (~0.2 mA board-level), but:

* `embassy-rp 0.10` offers no DORMANT API; entering it means stopping XOSC/PLLs
  by hand and waking via GPIO or the RTC with an external trigger.
* DORMANT tears down the CYW43439 association. Each wake then costs a full
  WiFi join + DHCP (estimated, not measured: 2–5 s at 40–80 mA), which over a 300 s cycle is in
  the same range as staying associated in PM `Aggressive`.

Staying associated is therefore the simpler and robust default. Before
investing in DORMANT, **measure** (USB power meter or shunt on VSYS) both
variants for your AP's DTIM interval.

Further cheap wins, not yet implemented:

1. Lower `clk_sys` (e.g. 48 MHz) — the node needs < 1 % CPU.
2. `control.leave()` + radio power-down for intervals ≫ 5 min.
3. Feed a watchdog from the main loop to recover from a wedged WiFi driver
   (needs a feeder shorter than the RP2040's 8.3 s max watchdog period).

## Transmission protocol: HTTP, not MQTT

| criterion | HTTP POST (chosen) | MQTT |
|---|---|---|
| sinks reachable without extra infra | Telegraf `http_listener_v2`, ThingSpeak, Node-RED, Home Assistant | requires a broker (Mosquitto) + subscriber |
| per-sample cost | 1 TCP connection, ~300 B request, socket closed afterwards | persistent connection + keep-alive pings → radio wakes between samples |
| firmware cost | ~80 lines hand-written (`http.rs` + `upload`), no extra crate | `rust-mqtt` (+ session state) |
| delivery confirmation | HTTP status code per sample | QoS 1 PUBACK |

At one message per 5 minutes the persistent MQTT session buys nothing and its
keep-alive traffic works against the power budget. MQTT becomes the better
fit for many nodes or bidirectional control (commands back to the node).

## Authentication & transport security

| sink | auth | how |
|---|---|---|
| Telegraf `http_listener_v2` | Basic | `HTTP_BASIC_USER/PASS` → `Authorization: Basic …` (base64 at build time) |
| Node-RED / HA / custom | Bearer | `API_KEY` → `Authorization: Bearer …` |
| ThingSpeak | write key | `API_KEY` in the form body |

**No TLS.** Options considered:

* `reqwless` + `embedded-tls`: feasible on RP2040 (~16–20 KiB extra RAM for
  record buffers), but certificate verification is not on by default — TLS
  without verification would only give a false sense of security.
* Recommended deployment instead: IoT VLAN / trusted WLAN, sink reachable only
  from that segment, write-only credentials dedicated to this node.

## Config & secrets flow

```
.env (git-ignored) ─┐
shell env ──────────┴─▶ build.rs ─ validate ─▶ cargo:rustc-env=CFG_* ─▶ env!() consts
                                                            │
                                            firmware ELF in target/ (git-ignored)
```
