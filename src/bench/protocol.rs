use std::io;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub const MAGIC: &[u8; 8] = b"PROXYBEN";
pub const VERSION: u8 = 0x01;

pub const KIND_TCP_UP: u8 = 0;
pub const KIND_TCP_DOWN: u8 = 1;
pub const KIND_UDP_UP: u8 = 2;
pub const KIND_UDP_DOWN: u8 = 3;
pub const KIND_CLOSE: u8 = 4;

pub const STATUS_OK: u8 = 0;
pub const STATUS_ERR: u8 = 1;

pub async fn write_handshake<W: AsyncWriteExt + Unpin>(w: &mut W) -> io::Result<()> {
    w.write_all(MAGIC).await?;
    w.write_u8(VERSION).await?;
    Ok(())
}

pub async fn read_handshake<R: AsyncReadExt + Unpin>(r: &mut R) -> io::Result<()> {
    let mut magic = [0u8; 8];
    r.read_exact(&mut magic).await?;
    if &magic != MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "bench handshake: bad magic",
        ));
    }
    let version = r.read_u8().await?;
    if version != VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("bench handshake: unsupported version {version}"),
        ));
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct TestRequest {
    pub kind: u8,
    pub duration_ms: u32,
    pub payload_size: u32,
    pub bandwidth_bps: u64,
    pub expected_packets: u64,
}

impl TestRequest {
    pub fn close() -> Self {
        Self {
            kind: KIND_CLOSE,
            duration_ms: 0,
            payload_size: 0,
            bandwidth_bps: 0,
            expected_packets: 0,
        }
    }

    pub async fn write<W: AsyncWriteExt + Unpin>(&self, w: &mut W) -> io::Result<()> {
        w.write_u8(self.kind).await?;
        w.write_u32(self.duration_ms).await?;
        w.write_u32(self.payload_size).await?;
        w.write_u64(self.bandwidth_bps).await?;
        w.write_u64(self.expected_packets).await?;
        Ok(())
    }

    pub async fn read<R: AsyncReadExt + Unpin>(r: &mut R) -> io::Result<Self> {
        Ok(Self {
            kind: r.read_u8().await?,
            duration_ms: r.read_u32().await?,
            payload_size: r.read_u32().await?,
            bandwidth_bps: r.read_u64().await?,
            expected_packets: r.read_u64().await?,
        })
    }

    pub fn is_udp(&self) -> bool {
        matches!(self.kind, KIND_UDP_UP | KIND_UDP_DOWN)
    }
}

#[derive(Debug, Clone)]
pub struct TestResponse {
    pub status: u8,
    pub data_port: u16,
    pub error: String,
}

impl TestResponse {
    pub fn ok(data_port: u16) -> Self {
        Self {
            status: STATUS_OK,
            data_port,
            error: String::new(),
        }
    }
    pub fn err(msg: impl Into<String>) -> Self {
        Self {
            status: STATUS_ERR,
            data_port: 0,
            error: msg.into(),
        }
    }

    pub async fn write<W: AsyncWriteExt + Unpin>(&self, w: &mut W) -> io::Result<()> {
        w.write_u8(self.status).await?;
        w.write_u16(self.data_port).await?;
        let bytes = self.error.as_bytes();
        let len: u16 = bytes.len().try_into().unwrap_or(u16::MAX);
        w.write_u16(len).await?;
        w.write_all(&bytes[..len as usize]).await?;
        Ok(())
    }

    pub async fn read<R: AsyncReadExt + Unpin>(r: &mut R) -> io::Result<Self> {
        let status = r.read_u8().await?;
        let data_port = r.read_u16().await?;
        let err_len = r.read_u16().await? as usize;
        let mut err_buf = vec![0u8; err_len];
        r.read_exact(&mut err_buf).await?;
        let error = String::from_utf8_lossy(&err_buf).into_owned();
        Ok(Self { status, data_port, error })
    }
}

#[derive(Debug, Clone, Default)]
pub struct TestReport {
    pub bytes: u64,
    pub packets: u64,
    pub max_seq: u64,
    pub ooo: u64,
    pub dup: u64,
    pub jitter_ns: u64,
    pub latency_min_ns: u64,
    pub latency_max_ns: u64,
    pub latency_sum_ns: u64,
    pub duration_actual_ns: u64,
}

impl TestReport {
    pub async fn write<W: AsyncWriteExt + Unpin>(&self, w: &mut W) -> io::Result<()> {
        w.write_u64(self.bytes).await?;
        w.write_u64(self.packets).await?;
        w.write_u64(self.max_seq).await?;
        w.write_u64(self.ooo).await?;
        w.write_u64(self.dup).await?;
        w.write_u64(self.jitter_ns).await?;
        w.write_u64(self.latency_min_ns).await?;
        w.write_u64(self.latency_max_ns).await?;
        w.write_u64(self.latency_sum_ns).await?;
        w.write_u64(self.duration_actual_ns).await?;
        Ok(())
    }

    pub async fn read<R: AsyncReadExt + Unpin>(r: &mut R) -> io::Result<Self> {
        Ok(Self {
            bytes: r.read_u64().await?,
            packets: r.read_u64().await?,
            max_seq: r.read_u64().await?,
            ooo: r.read_u64().await?,
            dup: r.read_u64().await?,
            jitter_ns: r.read_u64().await?,
            latency_min_ns: r.read_u64().await?,
            latency_max_ns: r.read_u64().await?,
            latency_sum_ns: r.read_u64().await?,
            duration_actual_ns: r.read_u64().await?,
        })
    }
}

/// Per-datagram payload header (16 bytes).
pub const UDP_HEADER_LEN: usize = 16;

pub fn encode_udp_header(buf: &mut [u8], seq: u64, send_ts_ns: u64) {
    buf[..8].copy_from_slice(&seq.to_be_bytes());
    buf[8..16].copy_from_slice(&send_ts_ns.to_be_bytes());
}

pub fn decode_udp_header(buf: &[u8]) -> Option<(u64, u64)> {
    if buf.len() < UDP_HEADER_LEN {
        return None;
    }
    let seq = u64::from_be_bytes(buf[..8].try_into().ok()?);
    let ts = u64::from_be_bytes(buf[8..16].try_into().ok()?);
    Some((seq, ts))
}
