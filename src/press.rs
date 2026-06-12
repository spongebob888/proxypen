use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::Semaphore;

use crate::config::TestTarget;
use crate::result::TestStatus;
use crate::transport::Transport;

/// Which protocol to press-test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PressProtocol {
    Http1,
    Http2,
    Http3,
}

impl std::fmt::Display for PressProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PressProtocol::Http1 => write!(f, "HTTP/1.1"),
            PressProtocol::Http2 => write!(f, "HTTP/2"),
            PressProtocol::Http3 => write!(f, "HTTP/3"),
        }
    }
}

/// Options for the press (latency retest) feature.
#[derive(Debug, Clone)]
pub struct PressOptions {
    pub transport: Transport,
    pub target: TestTarget,
    pub protocol: PressProtocol,
    pub concurrency: usize,
    pub num_requests: usize,
    pub timeout: Duration,
}

/// Latency statistics computed from collected samples.
#[derive(Debug, Clone)]
pub struct LatencyStats {
    pub min_ms: f64,
    pub max_ms: f64,
    pub avg_ms: f64,
    pub p50_ms: f64,
    pub p90_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub sample_count: usize,
}

impl Default for LatencyStats {
    fn default() -> Self {
        Self {
            min_ms: 0.0,
            max_ms: 0.0,
            avg_ms: 0.0,
            p50_ms: 0.0,
            p90_ms: 0.0,
            p95_ms: 0.0,
            p99_ms: 0.0,
            sample_count: 0,
        }
    }
}

impl LatencyStats {
    /// Compute statistics from a sorted slice of millisecond values.
    pub fn from_sorted_ms(samples: &[f64]) -> Self {
        if samples.is_empty() {
            return Self::default();
        }
        let n = samples.len();
        let sum: f64 = samples.iter().sum();
        Self {
            min_ms: samples[0],
            max_ms: samples[n - 1],
            avg_ms: sum / n as f64,
            p50_ms: percentile(samples, 50.0),
            p90_ms: percentile(samples, 90.0),
            p95_ms: percentile(samples, 95.0),
            p99_ms: percentile(samples, 99.0),
            sample_count: n,
        }
    }
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let n = sorted.len();
    let idx = (p / 100.0 * (n - 1) as f64) as usize;
    sorted[idx.clamp(0, n - 1)]
}

/// Result of a press run.
#[derive(Debug, Clone)]
pub struct PressResult {
    pub protocol: PressProtocol,
    pub total_requests: usize,
    pub success_count: usize,
    pub error_count: usize,
    pub ttfb: LatencyStats,
    pub total_time: LatencyStats,
    pub total_duration: Duration,
    pub requests_per_second: f64,
    pub errors: Vec<String>,
}

impl std::fmt::Display for PressResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "=== Press Test Results ===")?;
        writeln!(f, "Protocol:        {}", self.protocol)?;
        writeln!(f, "Total requests:  {}", self.total_requests)?;
        writeln!(f, "Successful:      {}", self.success_count)?;
        writeln!(f, "Failed:          {}", self.error_count)?;
        writeln!(
            f,
            "Duration:        {:.2}s",
            self.total_duration.as_secs_f64()
        )?;
        writeln!(f, "Req/sec:         {:.2}", self.requests_per_second)?;
        writeln!(f)?;
        writeln!(f, "--- TTFB (Time To First Byte) ---")?;
        writeln!(f, "  min: {:>8.2} ms", self.ttfb.min_ms)?;
        writeln!(f, "  avg: {:>8.2} ms", self.ttfb.avg_ms)?;
        writeln!(f, "  p50: {:>8.2} ms", self.ttfb.p50_ms)?;
        writeln!(f, "  p90: {:>8.2} ms", self.ttfb.p90_ms)?;
        writeln!(f, "  p95: {:>8.2} ms", self.ttfb.p95_ms)?;
        writeln!(f, "  p99: {:>8.2} ms", self.ttfb.p99_ms)?;
        writeln!(f, "  max: {:>8.2} ms", self.ttfb.max_ms)?;
        writeln!(f)?;
        writeln!(f, "--- Total Request Time ---")?;
        writeln!(f, "  min: {:>8.2} ms", self.total_time.min_ms)?;
        writeln!(f, "  avg: {:>8.2} ms", self.total_time.avg_ms)?;
        writeln!(f, "  p50: {:>8.2} ms", self.total_time.p50_ms)?;
        writeln!(f, "  p90: {:>8.2} ms", self.total_time.p90_ms)?;
        writeln!(f, "  p95: {:>8.2} ms", self.total_time.p95_ms)?;
        writeln!(f, "  p99: {:>8.2} ms", self.total_time.p99_ms)?;
        writeln!(f, "  max: {:>8.2} ms", self.total_time.max_ms)?;

        if !self.errors.is_empty() {
            writeln!(f)?;
            writeln!(f, "--- Errors ---")?;
            for (i, err) in self.errors.iter().enumerate() {
                if i >= 20 {
                    writeln!(f, "  ... and {} more", self.errors.len() - 20)?;
                    break;
                }
                writeln!(f, "  {err}")?;
            }
        }
        Ok(())
    }
}

/// Run a press/retest against the target with the given options.
pub async fn press(opts: &PressOptions) -> PressResult {
    let semaphore = Arc::new(Semaphore::new(opts.concurrency));
    let ttfb_samples: Arc<Mutex<Vec<f64>>> = Arc::new(Mutex::new(Vec::with_capacity(opts.num_requests)));
    let total_samples: Arc<Mutex<Vec<f64>>> = Arc::new(Mutex::new(Vec::with_capacity(opts.num_requests)));
    let success_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let error_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let errors: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    let start = Instant::now();
    let mut handles = Vec::with_capacity(opts.num_requests);

    for _ in 0..opts.num_requests {
        let transport = opts.transport.clone();
        let target = opts.target.clone();
        let protocol = opts.protocol;
        let timeout = opts.timeout;
        let sem = semaphore.clone();
        let ttfb = ttfb_samples.clone();
        let total = total_samples.clone();
        let sc = success_count.clone();
        let ec = error_count.clone();
        let errs = errors.clone();

        let handle = tokio::spawn(async move {
            let _permit = sem.acquire().await.expect("semaphore closed");
            let result = match protocol {
                PressProtocol::Http1 => {
                    crate::http1::test(&transport, &target, timeout).await
                }
                PressProtocol::Http2 => {
                    crate::http2::test(&transport, &target, timeout).await
                }
                PressProtocol::Http3 => {
                    crate::http3::test(&transport, &target, timeout).await
                }
            };

            match result.status {
                TestStatus::Success => {
                    let ttfb_ms = result.timing.first_byte.as_secs_f64() * 1000.0;
                    let total_ms = result.timing.total.as_secs_f64() * 1000.0;
                    ttfb.lock().unwrap().push(ttfb_ms);
                    total.lock().unwrap().push(total_ms);
                    sc.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                TestStatus::Failed(err) => {
                    ec.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let mut errs = errs.lock().unwrap();
                    if errs.len() < 50 {
                        errs.push(err);
                    }
                }
            }
        });
        handles.push(handle);
    }

    for handle in handles {
        let _ = handle.await;
    }

    let total_duration = start.elapsed();
    let sc = success_count.load(std::sync::atomic::Ordering::Relaxed);
    let rps = if total_duration.as_secs_f64() > 0.0 {
        opts.num_requests as f64 / total_duration.as_secs_f64()
    } else {
        0.0
    };

    let mut ttfb_vec = std::mem::take(&mut *ttfb_samples.lock().unwrap());
    let mut total_vec = std::mem::take(&mut *total_samples.lock().unwrap());
    ttfb_vec.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    total_vec.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    PressResult {
        protocol: opts.protocol,
        total_requests: opts.num_requests,
        success_count: sc,
        error_count: error_count.load(std::sync::atomic::Ordering::Relaxed),
        ttfb: LatencyStats::from_sorted_ms(&ttfb_vec),
        total_time: LatencyStats::from_sorted_ms(&total_vec),
        total_duration,
        requests_per_second: rps,
        errors: std::mem::take(&mut *errors.lock().unwrap()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- percentile ----

    #[test]
    fn percentile_empty_returns_zero() {
        assert_eq!(percentile(&[], 50.0), 0.0);
    }

    #[test]
    fn percentile_single() {
        assert_eq!(percentile(&[5.0], 0.0), 5.0);
        assert_eq!(percentile(&[5.0], 50.0), 5.0);
        assert_eq!(percentile(&[5.0], 99.0), 5.0);
        assert_eq!(percentile(&[5.0], 100.0), 5.0);
    }

    #[test]
    fn percentile_exact_p50() {
        // [0, 1, 2, 3]  —  p50 = idx (50/100 * 3) = 1 ⇒ 1.0
        assert_eq!(percentile(&[0.0, 1.0, 2.0, 3.0], 50.0), 1.0);
    }

    #[test]
    fn percentile_exact_p99() {
        // 100 samples: 0.0, 1.0, …, 99.0
        // p99 idx = (99/100)*99 = 98.01 → 98 ⇒ 98.0
        let data: Vec<f64> = (0..100).map(|i| i as f64).collect();
        assert_eq!(percentile(&data, 99.0), 98.0);
    }

    #[test]
    fn percentile_p50_100_samples() {
        // 100 samples: 1.0, 2.0, …, 100.0
        // p50 idx = (50/100)*99 = 49.5 → 49 ⇒ 50.0
        let data: Vec<f64> = (1..=100).map(|i| i as f64).collect();
        assert_eq!(percentile(&data, 50.0), 50.0);
    }

    // ---- LatencyStats ----

    #[test]
    fn latency_stats_default_is_all_zeros() {
        let s = LatencyStats::default();
        assert_eq!(s.min_ms, 0.0);
        assert_eq!(s.max_ms, 0.0);
        assert_eq!(s.avg_ms, 0.0);
        assert_eq!(s.p50_ms, 0.0);
        assert_eq!(s.p99_ms, 0.0);
        assert_eq!(s.sample_count, 0);
    }

    #[test]
    fn latency_stats_empty_slice_returns_default() {
        let s = LatencyStats::from_sorted_ms(&[]);
        assert_eq!(s.sample_count, 0);
        assert_eq!(s.min_ms, 0.0);
    }

    #[test]
    fn latency_stats_single_sample() {
        let s = LatencyStats::from_sorted_ms(&[7.5]);
        assert_eq!(s.sample_count, 1);
        assert_eq!(s.min_ms, 7.5);
        assert_eq!(s.max_ms, 7.5);
        assert_eq!(s.avg_ms, 7.5);
        assert_eq!(s.p50_ms, 7.5);
        assert_eq!(s.p99_ms, 7.5);
    }

    #[test]
    fn latency_stats_linear_1_to_100() {
        let data: Vec<f64> = (1..=100).map(|i| i as f64).collect();
        let s = LatencyStats::from_sorted_ms(&data);
        assert_eq!(s.sample_count, 100);
        assert_eq!(s.min_ms, 1.0);
        assert_eq!(s.max_ms, 100.0);
        // average of 1..100 = 5050/100 = 50.5
        assert!((s.avg_ms - 50.5).abs() < 0.01);
        assert_eq!(s.p50_ms, 50.0);  // idx 49
        assert_eq!(s.p90_ms, 90.0);  // idx 89
        assert_eq!(s.p95_ms, 95.0);  // idx 94
        assert_eq!(s.p99_ms, 99.0);  // idx 98
    }

    #[test]
    fn latency_stats_two_samples() {
        let s = LatencyStats::from_sorted_ms(&[2.0, 8.0]);
        assert_eq!(s.sample_count, 2);
        assert_eq!(s.min_ms, 2.0);
        assert_eq!(s.max_ms, 8.0);
        assert_eq!(s.avg_ms, 5.0);
        // p50 idx = (50/100)*1 = 0 ⇒ 2.0
        assert_eq!(s.p50_ms, 2.0);
        // p99 idx = (99/100)*1 = 0 ⇒ 2.0
        assert_eq!(s.p99_ms, 2.0);
    }

    // ---- PressProtocol::Display ----

    #[test]
    fn press_protocol_display() {
        assert_eq!(PressProtocol::Http1.to_string(), "HTTP/1.1");
        assert_eq!(PressProtocol::Http2.to_string(), "HTTP/2");
        assert_eq!(PressProtocol::Http3.to_string(), "HTTP/3");
    }

    // ---- PressResult::Display ----

    #[test]
    fn press_result_display_success() {
        let result = PressResult {
            protocol: PressProtocol::Http1,
            total_requests: 100,
            success_count: 100,
            error_count: 0,
            ttfb: LatencyStats::from_sorted_ms(&[1.0, 2.0, 3.0]),
            total_time: LatencyStats::from_sorted_ms(&[2.0, 3.0, 4.0]),
            total_duration: Duration::from_millis(250),
            requests_per_second: 400.0,
            errors: vec![],
        };
        let out = result.to_string();
        assert!(out.contains("Press Test Results"));
        assert!(out.contains("HTTP/1.1"));
        assert!(out.contains("100"));
        assert!(out.contains("0.25s"));
        assert!(out.contains("400.00"));
        assert!(out.contains("p50"));
        assert!(out.contains("p99"));
    }

    #[test]
    fn press_result_display_with_errors() {
        let result = PressResult {
            protocol: PressProtocol::Http3,
            total_requests: 10,
            success_count: 7,
            error_count: 3,
            ttfb: LatencyStats::from_sorted_ms(&[5.0]),
            total_time: LatencyStats::from_sorted_ms(&[6.0]),
            total_duration: Duration::from_secs(2),
            requests_per_second: 5.0,
            errors: vec!["timeout".into(), "connection refused".into()],
        };
        let out = result.to_string();
        assert!(out.contains("Failed:          3"));
        assert!(out.contains("Errors"));
        assert!(out.contains("timeout"));
        assert!(out.contains("connection refused"));
    }

    #[test]
    fn press_result_display_truncates_many_errors() {
        let errors: Vec<String> = (0..30).map(|i| format!("err {i}")).collect();
        let result = PressResult {
            protocol: PressProtocol::Http2,
            total_requests: 50,
            success_count: 20,
            error_count: 30,
            ttfb: LatencyStats::from_sorted_ms(&[1.0]),
            total_time: LatencyStats::from_sorted_ms(&[1.0]),
            total_duration: Duration::from_secs(1),
            requests_per_second: 50.0,
            errors,
        };
        let out = result.to_string();
        // Should contain first 20 errors, then truncation notice
        assert!(out.contains("err 0"));
        assert!(out.contains("err 19"));
        assert!(out.contains("and 10 more"));
        assert!(!out.contains("err 20"));
    }
}
