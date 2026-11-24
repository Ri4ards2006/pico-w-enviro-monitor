//! Host-testable core of the Pico W environmental node.
//!
//! * [`sensor`]  – reading type + plausibility gate (driver glue on ARM only)
//! * [`payload`] – JSON and ThingSpeak (url-encoded) body formatting
//! * [`http`]    – minimal HTTP/1.1 POST request builder + status-line parser
#![cfg_attr(not(test), no_std)]

pub mod http;
pub mod payload;
pub mod sensor;
