// The bench data plane uses the transport-aware UDP socket directly.
// Re-export with the historical name so callers don't need to know the
// implementation moved.

pub use crate::transport::TransportUdpSocket as BenchUdpSocket;
