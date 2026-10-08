//! The GPU's figures from `nvidia_gpu_exporter`'s Prometheus page, over `curl`
//!
//! The URL reaches curl as a config on its stdin, so no process listing
//! shows the host, and no error carries it.

use std::io::{Read, Write};
use std::process::Stdio;

use super::curl::render;
use crate::ports::{Gpu, GpuError, GpuMetrics};
use crate::settings::EndpointUrl;

/// Seconds curl gets to connect, and to finish the read
const CONNECT_TIMEOUT: u32 = 3;
const MAX_TIME: u32 = 5;

/// The most a metrics page may print: the exporter's page is a few kilobytes
const PAGE_MAX: usize = 1 << 20;

const UTILIZATION: &str = "nvidia_smi_utilization_gpu_ratio";
const MEMORY_USED: &str = "nvidia_smi_memory_used_bytes";
const MEMORY_TOTAL: &str = "nvidia_smi_memory_total_bytes";
const POWER: &str = "nvidia_smi_power_draw_watts";
const TEMPERATURE: &str = "nvidia_smi_temperature_gpu";

/// Reads the GPU's figures with the system's `curl`
#[derive(Debug, Clone, Copy, Default)]
pub struct GpuCurl;

impl GpuMetrics for GpuCurl {
    fn read(&self, url: &EndpointUrl) -> Result<Vec<Gpu>, GpuError> {
        let config = render(&[
            ("url", url.as_str().to_owned()),
            ("proto", "=https,http".to_owned()),
            ("connect-timeout", CONNECT_TIMEOUT.to_string()),
            ("max-time", MAX_TIME.to_string()),
            ("max-filesize", PAGE_MAX.to_string()),
            ("write-out", "\n%{http_code}".to_owned()),
        ]);
        let mut child = crate::spawn::command("curl")
            .args(["--config", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| GpuError::Spawn(e.to_string()))?;
        let mut stdin = child.stdin.take().expect("stdin was piped");
        let written = stdin.write_all(config.as_bytes());
        drop(stdin);
        // Reading stops past the cap, so a server that streams without end
        // fills no more than that.
        let mut bytes = Vec::new();
        let mut stdout = child.stdout.take().expect("stdout was piped");
        let read = stdout
            .by_ref()
            .take(PAGE_MAX as u64 + 16)
            .read_to_end(&mut bytes);
        let capped = bytes.len() > PAGE_MAX;
        if capped {
            let _ = child.kill();
        }
        let exit = child.wait().map_err(|e| GpuError::Spawn(e.to_string()))?;
        if capped {
            return Err(GpuError::Unreadable);
        }
        if written.is_err() || read.is_err() || !exit.success() {
            return Err(GpuError::Unreachable(exit.code().unwrap_or(-1)));
        }
        let text = String::from_utf8_lossy(&bytes);
        let (page, status) = text.rsplit_once('\n').unwrap_or(("", &text));
        match status.trim().parse::<u16>() {
            Ok(200..=299) => parse(page),
            Err(_) => Err(GpuError::Unreadable),
            Ok(status) => Err(GpuError::Refused(status)),
        }
    }
}

/// The GPUs on a Prometheus text page, in the order it first names them
///
/// # Errors
///
/// [`GpuError::Unreadable`] when no line is one of the five metrics read.
fn parse(page: &str) -> Result<Vec<Gpu>, GpuError> {
    let mut gpus: Vec<Gpu> = Vec::new();
    for line in page.lines() {
        let Some((name, labels, value)) = sample(line) else {
            continue;
        };
        let id = label(labels, "uuid");
        let figure = match name {
            UTILIZATION => Figure::Utilization(tenths(value * 100.0)),
            MEMORY_USED | MEMORY_TOTAL if value < 0.0 => continue,
            MEMORY_USED => Figure::Used(value.round() as u64),
            MEMORY_TOTAL => Figure::Total(value.round() as u64),
            POWER => Figure::Power(tenths(value)),
            TEMPERATURE => Figure::Temperature(tenths(value)),
            _ => continue,
        };
        let at = match gpus.iter().position(|gpu| gpu.id.as_deref() == id) {
            Some(at) => at,
            None => {
                gpus.push(Gpu {
                    id: id.map(str::to_owned),
                    utilization_percent: None,
                    memory_used_bytes: None,
                    memory_total_bytes: None,
                    power_watts: None,
                    temperature_celsius: None,
                });
                gpus.len() - 1
            }
        };
        let gpu = &mut gpus[at];
        match figure {
            Figure::Utilization(v) => gpu.utilization_percent = Some(v),
            Figure::Used(v) => gpu.memory_used_bytes = Some(v),
            Figure::Total(v) => gpu.memory_total_bytes = Some(v),
            Figure::Power(v) => gpu.power_watts = Some(v),
            Figure::Temperature(v) => gpu.temperature_celsius = Some(v),
        }
    }
    if gpus.is_empty() {
        return Err(GpuError::Unreadable);
    }
    Ok(gpus)
}

// A sample line is `name{labels} value` or `name value`, and may end in a
// timestamp. A comment, a blank line or a value that is not a number is none.
fn sample(line: &str) -> Option<(&str, &str, f64)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let (name, rest) = match line.find(['{', ' ']) {
        Some(at) if line.as_bytes()[at] == b'{' => {
            let close = line.rfind('}')?;
            (&line[..at], (&line[at + 1..close], &line[close + 1..]))
        }
        Some(at) => (&line[..at], ("", &line[at..])),
        None => return None,
    };
    let (labels, tail) = rest;
    let value: f64 = tail.split_whitespace().next()?.parse().ok()?;
    value.is_finite().then_some((name, labels, value))
}

// The quoted value of `key` among a sample's labels.
fn label<'a>(labels: &'a str, key: &str) -> Option<&'a str> {
    let own = format!("{key}=\"");
    let at = if labels.starts_with(&own) {
        0
    } else {
        labels.find(&format!(",{own}"))? + 1
    };
    let start = at + own.len();
    let end = labels[start..].find('"')?;
    Some(&labels[start..start + end])
}

fn tenths(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

// A reading, once its value has been accepted.
enum Figure {
    Utilization(f64),
    Used(u64),
    Total(u64),
    Power(f64),
    Temperature(f64),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test::StandInEndpoint;

    const PAGE: &str = include_str!("../../fixtures/gpu-exporter.txt");

    #[test]
    fn the_exporters_page_reads_as_one_gpu() {
        let gpus = parse(PAGE).unwrap();
        assert_eq!(
            gpus,
            [Gpu {
                id: Some("3f2a6c1e-0b7d-4c55-9d1a-52f1d0c4a7e8".into()),
                utilization_percent: Some(97.0),
                memory_used_bytes: Some(20_132_659_200),
                memory_total_bytes: Some(25_757_220_864),
                power_watts: Some(312.5),
                temperature_celsius: Some(71.0),
            }]
        );
    }

    #[test]
    fn a_second_gpu_is_told_apart_by_its_uuid() {
        let page = "nvidia_smi_temperature_gpu{uuid=\"a\"} 40\n\
                    nvidia_smi_temperature_gpu{uuid=\"b\"} 55 1727780000000\n\
                    nvidia_smi_power_draw_watts{uuid=\"b\"} 90\n";
        let gpus = parse(page).unwrap();
        assert_eq!(gpus.len(), 2);
        assert_eq!(gpus[0].temperature_celsius, Some(40.0));
        assert_eq!(
            (gpus[1].temperature_celsius, gpus[1].power_watts),
            (Some(55.0), Some(90.0))
        );
    }

    #[test]
    fn a_label_is_matched_whole_and_a_rejected_value_names_no_gpu() {
        let page = "nvidia_smi_temperature_gpu{mig_uuid=\"x\",uuid=\"a\"} 40\n";
        assert_eq!(parse(page).unwrap()[0].id.as_deref(), Some("a"));
        let negative = "nvidia_smi_memory_used_bytes{uuid=\"a\"} -1\n";
        assert_eq!(parse(negative), Err(GpuError::Unreadable));
    }

    #[test]
    fn a_page_without_the_gpu_metrics_is_unreadable() {
        for page in [
            "",
            "go_goroutines 8\n",
            "<html>not metrics</html>",
            "nvidia_smi_temperature_gpu NaN\n",
        ] {
            assert_eq!(parse(page), Err(GpuError::Unreadable), "{page:?}");
        }
    }

    #[test]
    fn a_stand_in_page_is_fetched_and_read() {
        let server = StandInEndpoint::start([]).with_metrics(PAGE);
        let url = EndpointUrl::try_from(server.metrics_url()).unwrap();
        let gpus = GpuCurl.read(&url).unwrap();
        assert_eq!(gpus[0].utilization_percent, Some(97.0));
        assert_eq!(server.seen(), ["GET /metrics HTTP/1.1"]);
    }

    #[test]
    fn a_server_that_refuses_or_is_not_there_says_so_without_its_url() {
        let refused = StandInEndpoint::start([]);
        let url = EndpointUrl::try_from(refused.metrics_url()).unwrap();
        assert_eq!(GpuCurl.read(&url), Err(GpuError::Refused(404)));
        let gone = EndpointUrl::try_from("http://127.0.0.1:1/metrics".to_owned()).unwrap();
        let error = GpuCurl.read(&gone).unwrap_err();
        assert!(matches!(error, GpuError::Unreachable(_)), "{error:?}");
        assert!(!error.to_string().contains("127.0.0.1"));
    }

    #[test]
    fn a_page_that_is_not_metrics_is_unreadable() {
        let server = StandInEndpoint::start([]).with_metrics("<html>hello</html>");
        let url = EndpointUrl::try_from(server.metrics_url()).unwrap();
        assert_eq!(GpuCurl.read(&url), Err(GpuError::Unreadable));
    }
}
