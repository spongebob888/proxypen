use std::time::{Duration, Instant};

/// Send-rate pacer. Computes the *cumulative* number of packets that should
/// have been sent by now and lets the caller batch sends within each
/// scheduler tick — this works around tokio's sub-millisecond sleep
/// granularity, which would otherwise cap throughput at ~1k pps.
pub struct Pacer {
    bandwidth_bps: u64,
    packet_bytes: usize,
    start: Instant,
}

impl Pacer {
    pub fn new(bandwidth_bps: u64, packet_bytes: usize) -> Self {
        Self {
            bandwidth_bps,
            packet_bytes,
            start: Instant::now(),
        }
    }

    /// How many packets should have been sent by `now`. `u64::MAX` when
    /// bandwidth is unlimited.
    pub fn target_packets_now(&self) -> u64 {
        if self.bandwidth_bps == 0 {
            return u64::MAX;
        }
        let elapsed_ns = self.start.elapsed().as_nanos();
        let bits_per_pkt = (self.packet_bytes as u128) * 8;
        if bits_per_pkt == 0 {
            return u64::MAX;
        }
        let total = (elapsed_ns * self.bandwidth_bps as u128) / (bits_per_pkt * 1_000_000_000);
        u64::try_from(total).unwrap_or(u64::MAX)
    }

    /// Sleep until packet number `nth` (0-indexed) is due.
    pub async fn sleep_until_packet(&self, nth: u64) {
        if self.bandwidth_bps == 0 {
            return;
        }
        let bits_per_pkt = (self.packet_bytes as u128) * 8;
        let target_ns = (nth as u128 * bits_per_pkt * 1_000_000_000) / self.bandwidth_bps as u128;
        let target_ns_u64 = u64::try_from(target_ns).unwrap_or(u64::MAX);
        let target = self.start + Duration::from_nanos(target_ns_u64);
        let now = Instant::now();
        if target > now {
            tokio::time::sleep_until(tokio::time::Instant::from_std(target)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unlimited_bandwidth_returns_max_target() {
        let p = Pacer::new(0, 1200);
        assert_eq!(p.target_packets_now(), u64::MAX);
    }

    #[test]
    fn target_packets_grows_with_time() {
        // 80 Mbit/s with 1000-byte packets ⇒ 10000 pps ⇒ 10 packets per ms.
        let p = Pacer::new(80_000_000, 1000);
        let before = p.target_packets_now();
        std::thread::sleep(Duration::from_millis(50));
        let after = p.target_packets_now();
        let delta = after - before;
        // Allow generous slack for scheduler jitter, but the order of
        // magnitude must be right (50ms × 10 pkt/ms ≈ 500).
        assert!(
            (300..=800).contains(&delta),
            "expected ~500 packets in 50ms, got {delta}"
        );
    }

    #[tokio::test]
    async fn sleep_until_packet_returns_after_due_time() {
        // 1 Mbit/s, 1000-byte packets ⇒ 125 packets/sec ⇒ 8 ms per packet.
        let p = Pacer::new(1_000_000, 1000);
        let t0 = Instant::now();
        p.sleep_until_packet(5).await; // due at ~40 ms
        let elapsed = t0.elapsed();
        assert!(
            elapsed >= Duration::from_millis(35),
            "sleep returned too early: {elapsed:?}"
        );
        assert!(
            elapsed < Duration::from_millis(200),
            "sleep ran way too long: {elapsed:?}"
        );
    }
}
