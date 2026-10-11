//! Shared status-metric data and sampling policy.
//!
//! Native collection lives in the selected `platform/<os>.rs` module. This
//! module deliberately contains no process spawning, network clients, or
//! platform APIs so UI rendering can consume a plain `AppState` snapshot.

use std::time::{Duration, Instant};

pub(crate) const STATUS_METRIC_REFRESH_INTERVAL: Duration = Duration::from_secs(2);
pub(crate) const STATUS_METRIC_STALE_AFTER: Duration = Duration::from_secs(4);
const STATUS_METRIC_REPAINT_INTERVAL: Duration = Duration::from_secs(4);

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct StatusMetrics {
    pub cpu_percent: Option<u8>,
    /// Logical core count, so a busy percentage can be read as cores.
    pub cpu_cores: Option<u16>,
    pub mem_used_gib: Option<f32>,
    pub mem_total_gib: Option<f32>,
    /// Percentage of the volume that actually fills up: the data volume on
    /// macOS, the root filesystem elsewhere. `None` when it cannot be read.
    pub disk_percent: Option<u8>,
    /// Absolute companions of `disk_percent`, for hover detail.
    pub disk_used_gib: Option<f32>,
    pub disk_total_gib: Option<f32>,
    pub hostname: String,
}

/// A metric earns its place on the bar only once it matters. The show/hide gap
/// is deliberate: a value hovering at the threshold would otherwise flicker
/// the segment in and out every sample.
pub(crate) const METRIC_SHOW_AT_PERCENT: u8 = 80;
pub(crate) const METRIC_HIDE_BELOW_PERCENT: u8 = 78;

/// Whether a threshold-gated metric segment is visible, given whether it was
/// visible before.
pub(crate) fn metric_segment_visible(percent: Option<u8>, was_visible: bool) -> bool {
    match percent {
        Some(percent) if was_visible => percent >= METRIC_HIDE_BELOW_PERCENT,
        Some(percent) => percent >= METRIC_SHOW_AT_PERCENT,
        None => false,
    }
}

/// Logical cores as the platform reports them; `None` only where even the
/// standard library cannot count them.
pub(crate) fn cpu_core_count() -> Option<u16> {
    std::thread::available_parallelism()
        .ok()
        .and_then(|count| u16::try_from(count.get()).ok())
}

#[derive(Debug, Clone)]
pub(crate) struct StatusMetricsSnapshot {
    pub metrics: StatusMetrics,
    pub sampled_at: Instant,
}

impl StatusMetricsSnapshot {
    pub(crate) fn is_stale_at(&self, now: Instant) -> bool {
        now.duration_since(self.sampled_at) >= STATUS_METRIC_STALE_AFTER
    }
}

#[derive(Debug)]
pub(crate) struct StatusMetricSampler {
    previous_cpu: Option<(u64, u64)>,
}

impl StatusMetricSampler {
    pub(crate) fn new() -> Self {
        Self { previous_cpu: None }
    }

    pub(super) fn cpu_percent(&mut self, idle: u64, total: u64) -> Option<u8> {
        let result = self
            .previous_cpu
            .and_then(|(previous_idle, previous_total)| {
                let idle_delta = idle.saturating_sub(previous_idle);
                let total_delta = total.saturating_sub(previous_total);
                if total_delta == 0 {
                    return None;
                }
                let busy = total_delta.saturating_sub(idle_delta) as f64;
                Some(
                    ((busy / total_delta as f64) * 100.0)
                        .round()
                        .clamp(0.0, 100.0) as u8,
                )
            });
        self.previous_cpu = Some((idle, total));
        result
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct StatusMetricRefresh {
    next_at: Instant,
    in_flight: bool,
    last_repaint_at: Option<Instant>,
}

impl StatusMetricRefresh {
    pub(crate) fn immediate(now: Instant) -> Self {
        Self {
            next_at: now,
            in_flight: false,
            last_repaint_at: None,
        }
    }

    pub(crate) fn deadline(self) -> Option<Instant> {
        (!self.in_flight).then_some(self.next_at)
    }

    pub(crate) fn begin(&mut self, now: Instant) -> bool {
        if self.in_flight || now < self.next_at {
            return false;
        }
        self.in_flight = true;
        self.next_at = now + STATUS_METRIC_REFRESH_INTERVAL;
        true
    }

    pub(crate) fn finish(&mut self) {
        self.in_flight = false;
    }

    pub(crate) fn finish_and_should_repaint(&mut self, sampled_at: Option<Instant>) -> bool {
        self.finish();
        let Some(sampled_at) = sampled_at else {
            return false;
        };
        if self.last_repaint_at.is_some_and(|previous| {
            sampled_at.duration_since(previous) < STATUS_METRIC_REPAINT_INTERVAL
        }) {
            return false;
        }
        self.last_repaint_at = Some(sampled_at);
        true
    }

    #[cfg(test)]
    pub(crate) fn in_flight(self) -> bool {
        self.in_flight
    }
}

pub(super) fn short_hostname(hostname: &str) -> String {
    hostname.split('.').next().unwrap_or(hostname).to_string()
}

#[cfg(test)]
pub(crate) fn status_metrics_fixture() -> StatusMetrics {
    StatusMetrics {
        cpu_percent: Some(12),
        cpu_cores: Some(8),
        mem_used_gib: Some(8.0),
        mem_total_gib: Some(16.0),
        disk_percent: Some(41),
        disk_used_gib: Some(200.0),
        disk_total_gib: Some(480.0),
        hostname: "testhost".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metric_refresh_cadence_is_immediate_bounded_and_retries() {
        // AC5: immediate first start, 2s cadence, max one in flight, and retry.
        let start = Instant::now();
        let mut refresh = StatusMetricRefresh::immediate(start);
        assert!(refresh.begin(start));
        assert!(refresh.in_flight());
        assert!(!refresh.begin(start + Duration::from_secs(2)));
        refresh.finish();
        assert!(!refresh.begin(start + Duration::from_millis(1999)));
        assert!(refresh.begin(start + Duration::from_secs(2)));
    }

    #[test]
    fn platform_sampling_source_contract_excludes_processes_and_network_clients() {
        fn region<'a>(source: &'a str, end: Option<&str>) -> &'a str {
            let source = source
                .split_once("pub(crate) fn sample_status_metrics")
                .expect("sampling entrypoint")
                .1;
            end.and_then(|marker| source.split_once(marker).map(|parts| parts.0))
                .unwrap_or(source)
        }

        let sources = [
            (
                "macos",
                region(
                    include_str!("macos.rs"),
                    Some("#[cfg(test)]\nmod status_metric_tests"),
                ),
            ),
            (
                "linux",
                region(
                    include_str!("linux.rs"),
                    Some("#[cfg(test)]\nmod status_metric_tests"),
                ),
            ),
            (
                "windows",
                region(
                    include_str!("windows.rs"),
                    Some("#[cfg(test)]\nmod status_metric_tests"),
                ),
            ),
            ("fallback", region(include_str!("fallback.rs"), None)),
            (
                "shared",
                include_str!("status_metrics.rs")
                    .split_once("#[cfg(test)]\nmod tests")
                    .expect("shared policy boundary")
                    .0,
            ),
        ];
        let forbidden = [
            concat!("Command", "::new"),
            concat!("std::process", "::Command"),
            concat!("req", "west"),
            concat!("ure", "q"),
            concat!("Tcp", "Stream"),
            concat!("http", "://"),
            concat!("https", "://"),
        ];

        for (name, source) in sources {
            for pattern in forbidden {
                assert!(
                    !source.contains(pattern),
                    "{name} status sampling contains {pattern}"
                );
            }
        }
    }

    #[test]
    fn metric_refresh_bounds_repaints_without_delaying_samples() {
        // AC5: 2s samples remain bounded while idle full-frame repaints are capped at 4s.
        let start = Instant::now();
        let mut refresh = StatusMetricRefresh::immediate(start);
        assert!(refresh.begin(start));
        assert!(refresh.finish_and_should_repaint(Some(start)));
        assert!(refresh.begin(start + STATUS_METRIC_REFRESH_INTERVAL));
        assert!(!refresh.finish_and_should_repaint(Some(start + STATUS_METRIC_REFRESH_INTERVAL)));
        assert!(refresh.begin(start + STATUS_METRIC_STALE_AFTER));
        assert!(refresh.finish_and_should_repaint(Some(start + STATUS_METRIC_STALE_AFTER)));
    }

    #[test]
    fn metric_stale_boundary_is_injected_and_exact() {
        // AC5: snapshot availability expires deterministically at the 4s boundary.
        let sampled_at = Instant::now();
        let snapshot = StatusMetricsSnapshot {
            metrics: status_metrics_fixture(),
            sampled_at,
        };
        assert!(
            !snapshot.is_stale_at(sampled_at + STATUS_METRIC_STALE_AFTER - Duration::from_nanos(1))
        );
        assert!(snapshot.is_stale_at(sampled_at + STATUS_METRIC_STALE_AFTER));
    }

    #[test]
    fn metric_unavailable_cpu_fallback_is_deterministic() {
        // AC3/AC5: CPU stays unavailable until a positive interval exists.
        let mut sampler = StatusMetricSampler::new();
        assert_eq!(sampler.cpu_percent(20, 100), None);
        assert_eq!(sampler.cpu_percent(20, 100), None);
        assert_eq!(sampler.cpu_percent(30, 140), Some(75));
    }
}
