//! Build script:
//! 1. Linker setup (memory.x + cortex-m-rt / embassy-rp / defmt scripts).
//! 2. Compile-time configuration from the process environment or the
//!    git-ignored `.env` (env wins). Values are validated here so that a typo
//!    fails the build instead of the node in the field, and so that no CR/LF
//!    can ever reach an HTTP header line.
use std::{collections::HashMap, env, fs, path::PathBuf};

fn main() {
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    fs::copy("memory.x", out.join("memory.x")).expect("memory.x missing");
    println!("cargo:rustc-link-search={}", out.display());
    println!("cargo:rerun-if-changed=memory.x");
    println!("cargo:rerun-if-changed=.env");
    println!("cargo:rustc-link-arg-bins=--nmagic");
    println!("cargo:rustc-link-arg-bins=-Tlink.x");
    println!("cargo:rustc-link-arg-bins=-Tlink-rp.x");
    println!("cargo:rustc-link-arg-bins=-Tdefmt.x");

    // Host-side unit tests (`cargo test-host`) need no configuration.
    if !env::var("TARGET").unwrap_or_default().starts_with("thumbv6m") {
        return;
    }

    let dotenv = parse_dotenv(&fs::read_to_string(".env").unwrap_or_default());
    let get = |key: &str| {
        println!("cargo:rerun-if-env-changed={key}");
        env::var(key).ok().or_else(|| dotenv.get(key).cloned()).filter(|v| !v.is_empty())
    };
    let required = |key: &str| {
        get(key).unwrap_or_else(|| panic!("{key} not set: `cp .env.example .env` and fill it in (or export {key})"))
    };
    let header_safe = |key: &str, v: &str| {
        assert!(v.bytes().all(|b| b.is_ascii_graphic()), "{key} must be printable ASCII without spaces");
    };

    let ssid = required("WIFI_SSID");
    let psk = get("WIFI_PSK").unwrap_or_default();
    let host = required("ENDPOINT_HOST");
    header_safe("ENDPOINT_HOST", &host);
    let port = get("ENDPOINT_PORT").unwrap_or_else(|| "80".into());
    assert!(port.parse::<u16>().is_ok_and(|p| p != 0), "ENDPOINT_PORT={port} invalid");
    let path = get("ENDPOINT_PATH").unwrap_or_else(|| "/".into());
    header_safe("ENDPOINT_PATH", &path);
    assert!(path.starts_with('/'), "ENDPOINT_PATH must start with '/'");
    let format = get("PAYLOAD_FORMAT").unwrap_or_else(|| "json".into());
    assert!(matches!(format.as_str(), "json" | "thingspeak"), "PAYLOAD_FORMAT must be json|thingspeak");
    let api_key = get("API_KEY").unwrap_or_default();
    header_safe("API_KEY", &api_key);
    assert!(format != "thingspeak" || !api_key.is_empty(), "PAYLOAD_FORMAT=thingspeak needs API_KEY");
    assert!(api_key.bytes().all(|b| b.is_ascii_alphanumeric() || b"-._~".contains(&b)), "API_KEY must be URL-safe");
    // Authorization header value, computed here so the firmware needs no base64.
    let basic = (get("HTTP_BASIC_USER"), get("HTTP_BASIC_PASS"));
    let auth = match (&basic, format.as_str()) {
        ((Some(u), Some(p)), _) => {
            assert!(!u.contains(':'), "HTTP_BASIC_USER must not contain ':'");
            format!("Basic {}", base64(format!("{u}:{p}").as_bytes()))
        }
        ((Some(_), None) | (None, Some(_)), _) => panic!("set both HTTP_BASIC_USER and HTTP_BASIC_PASS"),
        ((None, None), "json") if !api_key.is_empty() => format!("Bearer {api_key}"),
        _ => String::new(),
    };
    let node = get("NODE_ID").unwrap_or_else(|| "enviro-01".into());
    assert!(
        !node.is_empty() && node.len() <= 32 && node.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b)),
        "NODE_ID must be 1..32 chars of [A-Za-z0-9_-]"
    );
    let interval = get("INTERVAL_SECS").unwrap_or_else(|| "300".into());
    assert!(interval.parse::<u32>().is_ok_and(|s| s >= 10), "INTERVAL_SECS must be an integer >= 10");
    let addr = get("BME280_ADDR").unwrap_or_else(|| "0x76".into());
    assert!(matches!(addr.as_str(), "0x76" | "0x77"), "BME280_ADDR must be 0x76 or 0x77");

    for (k, v) in [
        ("WIFI_SSID", ssid.as_str()),
        ("WIFI_PSK", &psk),
        ("ENDPOINT_HOST", &host),
        ("ENDPOINT_PORT", &port),
        ("ENDPOINT_PATH", &path),
        ("PAYLOAD_FORMAT", &format),
        ("API_KEY", &api_key),
        ("AUTH_HEADER", &auth),
        ("NODE_ID", &node),
        ("INTERVAL_SECS", &interval),
        ("BME280_ADDR", &addr),
    ] {
        println!("cargo:rustc-env=CFG_{k}={v}");
    }
}

fn parse_dotenv(src: &str) -> HashMap<String, String> {
    src.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| {
            let v = v.trim();
            let v = v.strip_prefix('"').and_then(|v| v.strip_suffix('"')).unwrap_or(v);
            (k.trim().to_owned(), v.to_owned())
        })
        .collect()
}

/// RFC 4648 base64 with padding (build-time only).
fn base64(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in input.chunks(3) {
        let n = (u32::from(c[0]) << 16) | (u32::from(*c.get(1).unwrap_or(&0)) << 8) | u32::from(*c.get(2).unwrap_or(&0));
        for i in 0..4 {
            out.push(if i <= c.len() { T[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    out
}
