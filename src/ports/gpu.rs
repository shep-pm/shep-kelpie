//! The GPU's load, as a Prometheus metrics page reports it

use std::fmt;

use serde::Serialize;

use crate::settings::EndpointUrl;

/// One GPU's reading. A figure the page does not carry is left out.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Gpu {
    /// The GPU's `uuid` label, when the page names one
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// How busy it is, in percent
    #[serde(skip_serializing_if = "Option::is_none")]
    pub utilization_percent: Option<f64>,
    /// Memory in use, in bytes
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_used_bytes: Option<u64>,
    /// Memory it has, in bytes
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_total_bytes: Option<u64>,
    /// What it draws, in watts
    #[serde(skip_serializing_if = "Option::is_none")]
    pub power_watts: Option<f64>,
    /// Its core temperature, in degrees Celsius
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature_celsius: Option<f64>,
}

/// Reads the GPU's figures from a metrics page
pub trait GpuMetrics: Send + Sync {
    /// Every GPU on the page at `url`, in the order the page lists them
    ///
    /// # Errors
    ///
    /// [`GpuError`] when the page cannot be fetched, or holds no GPU figures.
    fn read(&self, url: &EndpointUrl) -> Result<Vec<Gpu>, GpuError>;
}

/// Why the GPU's figures could not be read
///
/// No variant carries the URL: the host is the maintainer's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GpuError {
    /// `curl` could not be started, with the OS's reason
    Spawn(String),
    /// Nothing answered: `curl` gave up with this exit code
    Unreachable(i32),
    /// The server answered with this HTTP status instead of the page
    Refused(u16),
    /// The page holds none of the GPU exporter's metrics
    Unreadable,
}

impl fmt::Display for GpuError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn(error) => write!(f, "cannot run curl: {error}"),
            Self::Unreachable(code) => write!(f, "nothing answered (curl exit {code})"),
            Self::Refused(status) => write!(f, "the server answered HTTP {status}"),
            Self::Unreadable => f.write_str("the page holds no GPU metrics"),
        }
    }
}

impl core::error::Error for GpuError {}
