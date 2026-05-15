use crate::bench::protocol::TestReport;

/// Accumulator for UDP receive-side statistics.
///
/// Latency is measured as `recv_ts_ns - send_ts_ns` using each side's local
/// monotonic clock counted from the test start. This is meaningful as a
/// *relative* measurement (compare two runs between the same hosts) but is
/// not absolute one-way delay — there's no clock sync.
#[derive(Debug, Default)]
pub struct UdpStats {
    pub bytes: u64,
    pub packets: u64,
    pub max_seq: u64,
    pub ooo: u64,
    pub dup: u64,

    // RFC 3550 jitter — running estimate of inter-arrival variance.
    pub jitter_ns: f64,
    last_transit_ns: Option<i64>,

    pub latency_min_ns: u64,
    pub latency_max_ns: u64,
    pub latency_sum_ns: u128,

    seen: std::collections::HashSet<u64>,
    have_max: bool,
}

impl UdpStats {
    pub fn record(&mut self, seq: u64, send_ts_ns: u64, recv_ts_ns: u64, payload_len: usize) {
        if !self.seen.insert(seq) {
            self.dup += 1;
            return;
        }
        self.bytes += payload_len as u64;
        self.packets += 1;

        if !self.have_max {
            self.max_seq = seq;
            self.have_max = true;
        } else if seq > self.max_seq {
            self.max_seq = seq;
        } else {
            self.ooo += 1;
        }

        // Latency
        let latency = recv_ts_ns.saturating_sub(send_ts_ns);
        if self.latency_min_ns == 0 || latency < self.latency_min_ns {
            self.latency_min_ns = latency;
        }
        if latency > self.latency_max_ns {
            self.latency_max_ns = latency;
        }
        self.latency_sum_ns = self.latency_sum_ns.saturating_add(latency as u128);

        // RFC 3550 jitter on transit difference
        let transit_ns = recv_ts_ns as i64 - send_ts_ns as i64;
        if let Some(last) = self.last_transit_ns {
            let d = (transit_ns - last).unsigned_abs() as f64;
            self.jitter_ns += (d - self.jitter_ns) / 16.0;
        }
        self.last_transit_ns = Some(transit_ns);
    }

    pub fn into_report(self, duration_actual_ns: u64) -> TestReport {
        TestReport {
            bytes: self.bytes,
            packets: self.packets,
            max_seq: self.max_seq,
            ooo: self.ooo,
            dup: self.dup,
            jitter_ns: self.jitter_ns as u64,
            latency_min_ns: self.latency_min_ns,
            latency_max_ns: self.latency_max_ns,
            latency_sum_ns: self.latency_sum_ns.try_into().unwrap_or(u64::MAX),
            duration_actual_ns,
        }
    }
}
