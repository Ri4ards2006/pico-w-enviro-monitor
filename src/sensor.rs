//! BME280 environmental sensor (I2C, forced mode).
//!
//! The pure part ([`Reading`], [`Reading::is_plausible`]) is host-tested; the
//! I2C driver glue only builds for the RP2040.

/// One compensated sample.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Reading {
    pub temperature_c: f32,
    pub humidity_pct: f32,
    pub pressure_hpa: f32,
}

impl Reading {
    /// Reject values outside the BME280's specified operating range — the
    /// usual symptom of a loose wire, a BMP280 (no humidity) or a bus glitch.
    pub fn is_plausible(&self) -> bool {
        (-40.0..=85.0).contains(&self.temperature_c)
            && (0.0..=100.0).contains(&self.humidity_pct)
            && (300.0..=1100.0).contains(&self.pressure_hpa)
    }
}

#[cfg(target_arch = "arm")]
pub use driver::{Bme280, SensorError};

#[cfg(target_arch = "arm")]
mod driver {
    use bme280_rs::{AsyncBme280, Configuration, Filter, Oversampling, SensorMode};
    use embassy_rp::i2c::{Async, I2c};
    use embassy_rp::peripherals::I2C0;
    use embassy_time::Delay;

    use super::Reading;

    #[derive(Debug, defmt::Format)]
    pub enum SensorError {
        Bus,
        Disabled,
        Implausible,
    }

    /// BME280 on I2C0 in **forced mode**, Bosch's "weather monitoring" profile
    /// (datasheet §3.5.1): 1x oversampling for T/p/RH, IIR filter off. Each
    /// `read` triggers one conversion (~8 ms); afterwards the sensor returns
    /// to sleep mode (~0.1 µA) on its own.
    pub struct Bme280 {
        dev: AsyncBme280<I2c<'static, I2C0, Async>, Delay>,
    }

    impl Bme280 {
        pub fn new(i2c: I2c<'static, I2C0, Async>, secondary_addr: bool) -> Self {
            let addr = if secondary_addr { 0x77 } else { bme280_rs::DEFAULT_ADDRESS };
            Self { dev: AsyncBme280::new_with_address(i2c, addr, Delay) }
        }

        /// Read calibration NVM and program the sampling profile. Safe to retry.
        pub async fn init(&mut self) -> Result<(), SensorError> {
            self.dev.init().await.map_err(|_| SensorError::Bus)?;
            let cfg = Configuration::default()
                .with_temperature_oversampling(Oversampling::Oversample1)
                .with_pressure_oversampling(Oversampling::Oversample1)
                .with_humidity_oversampling(Oversampling::Oversample1)
                .with_filter(Filter::Off)
                .with_sensor_mode(SensorMode::Forced);
            self.dev.set_sampling_configuration(cfg).await.map_err(|_| SensorError::Bus)
        }

        pub async fn read(&mut self) -> Result<Reading, SensorError> {
            self.dev.take_forced_measurement().await.map_err(|_| SensorError::Bus)?;
            let s = self.dev.read_sample().await.map_err(|_| SensorError::Bus)?;
            let r = Reading {
                temperature_c: s.temperature.ok_or(SensorError::Disabled)?,
                humidity_pct: s.humidity.ok_or(SensorError::Disabled)?, // None on a BMP280
                pressure_hpa: s.pressure.ok_or(SensorError::Disabled)? / 100.0,
            };
            if r.is_plausible() { Ok(r) } else { Err(SensorError::Implausible) }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(t: f32, h: f32, p: f32) -> Reading {
        Reading { temperature_c: t, humidity_pct: h, pressure_hpa: p }
    }

    #[test]
    fn typical_indoor_reading_is_plausible() {
        assert!(r(21.5, 45.0, 1013.25).is_plausible());
    }

    #[test]
    fn out_of_range_values_are_rejected() {
        assert!(!r(-41.0, 45.0, 1013.0).is_plausible());
        assert!(!r(21.0, 100.5, 1013.0).is_plausible());
        assert!(!r(21.0, 45.0, 0.0).is_plausible()); // dead bus / no calibration
        assert!(!r(f32::NAN, 45.0, 1013.0).is_plausible());
    }
}
