//! Pico W environmental node firmware.
//!
//! BME280 (I2C0, forced mode) -> HTTP POST (JSON or ThingSpeak form) over
//! WiFi every `INTERVAL_SECS`. Between samples the CPU sits in WFE (embassy
//! executor idle), the CYW43439 stays associated in `Aggressive` power-save
//! and the BME280 is asleep.
#![no_std]
#![no_main]

use core::net::Ipv4Addr;

use cyw43::{JoinOptions, aligned_bytes};
use cyw43_pio::{DEFAULT_CLOCK_DIVIDER, PioSpi};
use defmt::{info, unwrap, warn};
use embassy_executor::Spawner;
use embassy_net::dns::DnsQueryType;
use embassy_net::tcp::TcpSocket;
use embassy_net::{Config, IpAddress, Stack, StackResources};
use embassy_rp::clocks::RoscRng;
use embassy_rp::gpio::{Level, Output};
use embassy_rp::peripherals::{DMA_CH0, I2C0, PIO0};
use embassy_rp::pio::{InterruptHandler, Pio};
use embassy_rp::{bind_interrupts, dma, i2c};
use embassy_time::{Duration, Instant, Ticker, Timer, with_timeout};
use embedded_io_async::Write;
use pico_w_enviro_monitor::payload::{self, Format};
use pico_w_enviro_monitor::{http, sensor::Bme280};
use static_cell::StaticCell;
use {defmt_rtt as _, panic_probe as _};

bind_interrupts!(struct Irqs {
    PIO0_IRQ_0 => InterruptHandler<PIO0>;
    DMA_IRQ_0 => dma::InterruptHandler<DMA_CH0>;
    I2C0_IRQ => i2c::InterruptHandler<I2C0>;
});

// ---- compile-time configuration (build.rs, from env / git-ignored .env) ----
const WIFI_SSID: &str = env!("CFG_WIFI_SSID");
const WIFI_PSK: &str = env!("CFG_WIFI_PSK");
const ENDPOINT_HOST: &str = env!("CFG_ENDPOINT_HOST");
const ENDPOINT_PORT: u16 = parse_u32(env!("CFG_ENDPOINT_PORT")) as u16;
const ENDPOINT_PATH: &str = env!("CFG_ENDPOINT_PATH");
const API_KEY: &str = env!("CFG_API_KEY");
/// Full `Authorization` value (`Basic …` / `Bearer …`) or empty; see build.rs.
const AUTH_HEADER: &str = env!("CFG_AUTH_HEADER");
const NODE_ID: &str = env!("CFG_NODE_ID");
const INTERVAL: Duration = Duration::from_secs(parse_u32(env!("CFG_INTERVAL_SECS")) as u64);
const BME280_SECONDARY: bool = matches!(env!("CFG_BME280_ADDR").as_bytes(), b"0x77");
const FORMAT: Format = match Format::parse(env!("CFG_PAYLOAD_FORMAT")) {
    Some(f) => f,
    None => panic!("invalid PAYLOAD_FORMAT"),
};

/// Hard ceiling for DNS + connect + request + response of one upload.
const UPLOAD_TIMEOUT: Duration = Duration::from_secs(20);

const fn parse_u32(s: &str) -> u32 {
    let b = s.as_bytes();
    let (mut i, mut v) = (0, 0u32);
    while i < b.len() {
        v = v * 10 + (b[i] - b'0') as u32;
        i += 1;
    }
    v
}

#[derive(Debug, defmt::Format)]
enum UploadError {
    Dns,
    Connect,
    Io,
    Format,
    Timeout,
    Status(u16),
}

#[embassy_executor::task]
async fn cyw43_task(runner: cyw43::Runner<'static, cyw43::SpiBus<Output<'static>, PioSpi<'static, PIO0, 0>>>) -> ! {
    runner.run().await
}

#[embassy_executor::task]
async fn net_task(mut runner: embassy_net::Runner<'static, cyw43::NetDriver<'static>>) -> ! {
    runner.run().await
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let p = embassy_rp::init(Default::default());
    info!("pico-w-enviro-monitor: node {}, interval {} s", NODE_ID, INTERVAL.as_secs());

    // ---- BME280 on I2C0: GP4 = SDA, GP5 = SCL ----
    let i2c = i2c::I2c::new_async(p.I2C0, p.PIN_5, p.PIN_4, Irqs, i2c::Config::default());
    let mut sensor = Bme280::new(i2c, BME280_SECONDARY);
    while let Err(e) = sensor.init().await {
        warn!("BME280 init failed ({}), check wiring / BME280_ADDR; retry in 5 s", e);
        Timer::after_secs(5).await;
    }

    // ---- CYW43439 WiFi ----
    let fw = aligned_bytes!("../cyw43-firmware/43439A0.bin");
    let clm = aligned_bytes!("../cyw43-firmware/43439A0_clm.bin");
    let nvram = aligned_bytes!("../cyw43-firmware/nvram_rp2040.bin");

    let pwr = Output::new(p.PIN_23, Level::Low);
    let cs = Output::new(p.PIN_25, Level::High);
    let mut pio = Pio::new(p.PIO0, Irqs);
    let spi = PioSpi::new(
        &mut pio.common,
        pio.sm0,
        DEFAULT_CLOCK_DIVIDER,
        pio.irq0,
        cs,
        p.PIN_24,
        p.PIN_29,
        dma::Channel::new(p.DMA_CH0, Irqs),
    );

    static STATE: StaticCell<cyw43::State> = StaticCell::new();
    let (net_device, mut control, runner) = cyw43::new(STATE.init(cyw43::State::new()), pwr, spi, fw, nvram).await;
    spawner.spawn(unwrap!(cyw43_task(runner)));
    control.init(clm).await;
    // Strongest officially supported PM mode: radio sleeps between DTIM beacons.
    control.set_power_management(cyw43::PowerManagementMode::Aggressive).await;

    static RESOURCES: StaticCell<StackResources<3>> = StaticCell::new();
    let (stack, runner) = embassy_net::new(
        net_device,
        Config::dhcpv4(Default::default()),
        RESOURCES.init(StackResources::new()),
        RoscRng.next_u64(),
    );
    spawner.spawn(unwrap!(net_task(runner)));

    // ---- sample / upload loop on a fixed cadence ----
    let mut ticker = Ticker::every(INTERVAL);
    loop {
        if !stack.is_link_up() {
            join(&mut control, stack).await;
        }
        match sensor.read().await {
            Ok(r) => {
                info!("T={} C RH={} % p={} hPa", r.temperature_c, r.humidity_pct, r.pressure_hpa);
                match payload::render(FORMAT, &r, NODE_ID, API_KEY, Instant::now().as_secs()) {
                    Ok(body) => match with_timeout(UPLOAD_TIMEOUT, upload(stack, body.as_bytes())).await {
                        Ok(Ok(code)) => info!("upload ok: HTTP {}", code),
                        Ok(Err(e)) => warn!("upload failed: {}", e),
                        Err(_) => warn!("upload failed: {}", UploadError::Timeout),
                    },
                    Err(_) => warn!("upload failed: {}", UploadError::Format),
                }
            }
            Err(e) => warn!("sensor read failed: {}", e),
        }
        ticker.next().await; // CPU idles in WFE until the next tick
    }
}

async fn join(control: &mut cyw43::Control<'static>, stack: Stack<'static>) {
    let opts = if WIFI_PSK.is_empty() {
        JoinOptions::new_open()
    } else {
        JoinOptions::new(WIFI_PSK.as_bytes())
    };
    let mut backoff = 2;
    while let Err(e) = control.join(WIFI_SSID, opts.clone()).await {
        warn!("join failed: {:?}, retry in {} s", e, backoff);
        Timer::after_secs(backoff).await;
        backoff = (backoff * 2).min(60);
    }
    stack.wait_config_up().await;
    if let Some(cfg) = stack.config_v4() {
        info!("WiFi up, DHCP lease {}", cfg.address);
    }
}

async fn upload(stack: Stack<'static>, body: &[u8]) -> Result<u16, UploadError> {
    let addr = match ENDPOINT_HOST.parse::<Ipv4Addr>() {
        Ok(ip) => IpAddress::Ipv4(ip),
        Err(_) => *stack
            .dns_query(ENDPOINT_HOST, DnsQueryType::A)
            .await
            .map_err(|_| UploadError::Dns)?
            .first()
            .ok_or(UploadError::Dns)?,
    };

    let mut rx = [0u8; 512];
    let mut tx = [0u8; 1024];
    let mut socket = TcpSocket::new(stack, &mut rx, &mut tx);
    socket.set_timeout(Some(Duration::from_secs(10)));
    socket.connect((addr, ENDPOINT_PORT)).await.map_err(|_| UploadError::Connect)?;

    let head = http::request_head(ENDPOINT_HOST, ENDPOINT_PATH, FORMAT.content_type(), AUTH_HEADER, body.len())
        .map_err(|_| UploadError::Format)?;
    socket.write_all(head.as_bytes()).await.map_err(|_| UploadError::Io)?;
    socket.write_all(body).await.map_err(|_| UploadError::Io)?;
    socket.flush().await.map_err(|_| UploadError::Io)?;

    let mut resp = [0u8; 32];
    let mut n = 0;
    let status = loop {
        if let Some(code) = http::parse_status(&resp[..n]) {
            break code;
        }
        match socket.read(&mut resp[n..]).await {
            Ok(0) | Err(_) => return Err(UploadError::Io),
            Ok(k) => n += k,
        }
        if n == resp.len() {
            return Err(UploadError::Io);
        }
    };
    socket.close();
    let _ = socket.flush().await;

    if (200..300).contains(&status) { Ok(status) } else { Err(UploadError::Status(status)) }
}
