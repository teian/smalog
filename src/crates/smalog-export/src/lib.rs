//! Export formats for smalog.
//!
//! This crate owns the implemented CSV and MQTT exporters, their
//! configuration, and the catalog of export formats whose adapters are still
//! planned. It deliberately has no dependency on storage or the service.

pub mod config;
pub mod csv;
pub mod error;
pub mod metrics;
pub mod mqtt;
pub mod planned;
mod view;

pub use config::{CsvConfig, MqttConfig, SpotTimeSource};
pub use csv::CsvWriter;
pub use error::{Error, Result};
pub use mqtt::{Publisher as MqttPublisher, SunTimes};
pub use planned::{ExportCapability, ExportStatus, ExportTarget, EXPORT_CAPABILITIES};
