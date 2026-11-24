# pico-w-enviro-monitor

Wireless environmental sensor node on a **Raspberry Pi Pico W**: a **BME280**
measures temperature, relative humidity and pressure; every 5 minutes the node
POSTs the reading over WiFi to an HTTP endpoint — as **JSON** (Telegraf →
InfluxDB → Grafana, Node-RED, Home Assistant, …) or as a **ThingSpeak**
channel update. `no_std` Rust on the embassy async executor.

```
┌──────────┐ I2C0 ┌──────────────────────────────┐  WiFi / HTTP POST  ┌───────────────┐
│  BME280  │◀────▶│ Pico W                       │───────────────────▶│ Telegraf /    │
│ forced   │      │ embassy · embassy-net · cyw43│    every 300 s     │ ThingSpeak /  │
│ mode     │      │ CPU in WFE between samples   │                    │ webhook       │
└──────────┘      └──────────────────────────────┘                    └──────┬────────┘
                                                                             ▼ Grafana
```

## Crates

| crate | version | role |
|---|---|---|
| `embassy-executor` / `embassy-time` | 0.10 / 0.5 | async executor, `Ticker`, timeouts |
| `embassy-rp` | 0.10 | RP2040 HAL: async I2C, PIO, DMA |
| `embassy-net` | 0.9 | smoltcp: DHCPv4, DNS, TCP |
| `cyw43` + `cyw43-pio` | 0.7 / 0.10 | CYW43439 WiFi driver |
| **`bme280-rs`** | 0.4 | async BME280 driver, forced mode, stable Rust |
| `heapless` | 0.9 | fixed-capacity strings for HTTP head/body |

## Wiring (Pico W ↔ BME280 breakout)

```
        Pico W (top view, USB up)                     BME280 breakout
   ┌─────────────────────────────┐
   │ pin 6  GP4  (I2C0 SDA) ●────┼────────────────────▶ SDA
   │ pin 7  GP5  (I2C0 SCL) ●────┼────────────────────▶ SCL
   │ pin 8  GND             ●────┼────────────────────▶ GND
   │ pin 36 3V3(OUT)        ●────┼────────────────────▶ VIN / VCC
   └─────────────────────────────┘                     SDO → GND  (addr 0x76, default)
                                                       CSB → 3V3  (I2C mode; on most breakouts already)
```

| BME280 | Pico W pin | GPIO / net |
|---|---|---|
| VIN / VCC | 36 | 3V3(OUT) — **never VBUS/5 V on bare 3.3 V modules** |
| GND | 8 (or 3, 13, 18, 23, 28, 38) | GND |
| SDA | 6 | GP4 · I2C0 SDA |
| SCL | 7 | GP5 · I2C0 SCL |
| SDO | — | GND → `0x76`, 3V3 → `0x77` (`BME280_ADDR`) |

Breakouts normally carry 4.7–10 kΩ pull-ups on SDA/SCL; bare sensors need them
added. Keep the sensor a few cm away from the Pico W (the RP2040 and the
radio warm the board by ~1–2 °C).

## Configuration

All configuration is compile-time and lives **outside the tree**:

```sh
cp .env.example .env   # git-ignored; build.rs reads + validates it
```

| key | default | |
|---|---|---|
| `WIFI_SSID` / `WIFI_PSK` | — | required / empty = open |
| `ENDPOINT_HOST` / `_PORT` / `_PATH` | — / 80 / `/` | hostname (DNS) or IPv4 literal |
| `PAYLOAD_FORMAT` | `json` | `json` or `thingspeak` |
| `API_KEY` | — | JSON: Bearer token · ThingSpeak: channel write key |
| `HTTP_BASIC_USER` / `_PASS` | — | Basic auth (Telegraf), base64 encoded at build time |
| `NODE_ID` | `enviro-01` | |
| `INTERVAL_SECS` | 300 | ≥ 10 |
| `BME280_ADDR` | `0x76` | or `0x77` |

Invalid values (missing SSID, CR/LF or spaces in host/path, ThingSpeak without
key, interval < 10 s, …) fail the **build**, not the node.

### Payloads

```http
POST /enviro HTTP/1.1
Content-Type: application/json

{"node":"enviro-01","temperature_c":21.53,"humidity_pct":45.20,"pressure_hpa":1013.25,"uptime_s":1234}
```

ThingSpeak: `api_key=<KEY>&field1=<°C>&field2=<%RH>&field3=<hPa>` to the
channel update path of the ThingSpeak API host.

### Telegraf → InfluxDB → Grafana (example)

```toml
[[inputs.http_listener_v2]]
  service_address = ":8090"          # must be reachable from the WiFi LAN
  paths           = ["/enviro"]
  methods         = ["POST"]
  basic_username  = "enviro"         # = HTTP_BASIC_USER
  basic_password  = "change-me"      # = HTTP_BASIC_PASS
  data_format     = "json"
  name_override   = "enviro"
  tag_keys        = ["node"]
```

## Build & flash

Requirements: Rust ≥ 1.85 with `thumbv6m-none-eabi`, `probe-rs`, a SWD probe
(Raspberry Pi Debug Probe or a second Pico with picoprobe).

```sh
rustup target add thumbv6m-none-eabi
cargo install probe-rs-tools --locked

cp .env.example .env && $EDITOR .env
cargo run --release        # build, flash over SWD, defmt logs via RTT
cargo test-host            # host unit tests: plausibility, payloads, HTTP framing
```

RTT log of a healthy node:

```
INFO  WiFi up, DHCP lease <ip>/24
INFO  T=21.53 C RH=45.2 % p=1013.25 hPa
INFO  upload ok: HTTP 204
```

## Layout

```
src/main.rs      WiFi join + reconnect, sample/upload loop, HTTP POST over TCP
src/sensor.rs    BME280 driver glue (forced mode) + plausibility gate
src/payload.rs   JSON / ThingSpeak body rendering
src/http.rs      HTTP/1.1 request head + status-line parser
build.rs         linker setup + validated compile-time config from env / .env
cyw43-firmware/  CYW43439 firmware/CLM/NVRAM (Infineon permissive licence)
docs/ARCHITECTURE.md  power management + transmission flow
```

## Security notes

* No SSIDs, passwords, keys, IPs or hostnames are in the repository; they are
  only compiled into the firmware image under `target/` (git-ignored).
* **Transport is plain HTTP.** Credentials (Basic/Bearer/ThingSpeak key)
  travel unencrypted over the WiFi segment — use a trusted LAN / IoT VLAN and a
  dedicated write-only key. TLS is a known gap, see `docs/ARCHITECTURE.md`.
