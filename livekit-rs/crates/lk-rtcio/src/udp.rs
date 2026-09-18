//! The real-socket transport: one bound UDP socket, batched reads and writes.
//!
//! `quinn-udp` is used for the batching rather than a plain `recv_from` loop.
//! At LiveKit's packet rates a syscall per datagram is the dominant cost, and
//! GRO on receive plus GSO on send collapses a burst to one syscall each way.
//!
//! This is the only part of `lk-rtcio` that touches the operating system;
//! everything above it takes [`Datagram`]s, which is what lets [`Shard`] run
//! unchanged over [`crate::vnet`].
//!
//! [`Shard`]: crate::Shard

use std::io;
use std::net::SocketAddr;

use bytes::BytesMut;
use quinn_udp::{RecvMeta, Transmit, UdpSockRef, UdpSocketState};
use tokio::net::UdpSocket;

use crate::datagram::{Datagram, Proto};

/// How many datagrams one `recv` syscall may return.
const BATCH_SIZE: usize = 32;
/// Receive buffer per slot. Large enough for a GRO-coalesced run of
/// MTU-sized datagrams.
const RECV_SLOT: usize = 64 * 1024;

/// A bound UDP socket with batched I/O.
pub struct UdpTransport {
    socket: UdpSocket,
    state: UdpSocketState,
    local_addr: SocketAddr,
    recv_buf: Box<[u8]>,
    metas: Box<[RecvMeta]>,
}

impl UdpTransport {
    /// Bind `addr` and prepare batched I/O on it.
    ///
    /// # Errors
    ///
    /// Any bind or socket-option failure.
    pub fn bind(addr: SocketAddr) -> io::Result<Self> {
        let socket = std::net::UdpSocket::bind(addr)?;
        socket.set_nonblocking(true)?;
        let state = UdpSocketState::new(UdpSockRef::from(&socket))?;
        let local_addr = socket.local_addr()?;
        let socket = UdpSocket::from_std(socket)?;

        Ok(Self {
            socket,
            state,
            local_addr,
            recv_buf: vec![0u8; RECV_SLOT * BATCH_SIZE].into_boxed_slice(),
            metas: vec![RecvMeta::default(); BATCH_SIZE].into_boxed_slice(),
        })
    }

    /// The address actually bound, with any ephemeral port resolved.
    #[must_use]
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Whether the kernel accepted GSO, and how large a batch it will take.
    ///
    /// Zero or one means every datagram costs its own syscall; the send path
    /// still works, it is just slower.
    #[must_use]
    pub fn max_gso_segments(&self) -> usize {
        self.state.max_gso_segments()
    }

    /// Wait for readability and return a batch of datagrams (at most 32).
    ///
    /// GRO may coalesce several datagrams into one slot; they are split back
    /// out here, so the caller always sees one [`Datagram`] per datagram sent.
    ///
    /// # Errors
    ///
    /// Any receive failure other than `WouldBlock`, which is retried.
    pub async fn recv_batch(&mut self, out: &mut Vec<Datagram>) -> io::Result<usize> {
        loop {
            self.socket.readable().await?;

            let mut bufs: Vec<io::IoSliceMut<'_>> = self
                .recv_buf
                .chunks_mut(RECV_SLOT)
                .map(io::IoSliceMut::new)
                .collect();

            let result = self.socket.try_io(tokio::io::Interest::READABLE, || {
                self.state
                    .recv(UdpSockRef::from(&self.socket), &mut bufs, &mut self.metas)
            });

            let count = match result {
                Ok(count) => count,
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => continue,
                Err(err) => return Err(err),
            };

            let mut produced = 0;
            for (meta, buf) in self
                .metas
                .iter()
                .zip(self.recv_buf.chunks(RECV_SLOT))
                .take(count)
            {
                if meta.len == 0 {
                    continue;
                }
                let stride = if meta.stride == 0 {
                    meta.len
                } else {
                    meta.stride
                };
                let Some(data) = buf.get(..meta.len) else {
                    continue;
                };
                for segment in data.chunks(stride) {
                    out.push(Datagram {
                        peer: meta.addr,
                        local: meta.dst_ip.map_or(self.local_addr, |ip| {
                            SocketAddr::new(ip, self.local_addr.port())
                        }),
                        proto: Proto::Udp,
                        payload: BytesMut::from(segment),
                    });
                    produced += 1;
                }
            }
            return Ok(produced);
        }
    }

    /// Send one datagram.
    ///
    /// # Errors
    ///
    /// Any send failure other than `WouldBlock`, which is retried.
    pub async fn send(&self, datagram: &Datagram) -> io::Result<()> {
        loop {
            self.socket.writable().await?;
            let transmit = Transmit {
                destination: datagram.peer,
                ecn: None,
                contents: &datagram.payload,
                segment_size: None,
                src_ip: None,
            };
            match self.socket.try_io(tokio::io::Interest::WRITABLE, || {
                self.state
                    .try_send(UdpSockRef::from(&self.socket), &transmit)
            }) {
                Ok(()) => return Ok(()),
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => continue,
                Err(err) => return Err(err),
            }
        }
    }

    /// Send a run of datagrams that share a destination as one GSO batch.
    ///
    /// Returns how many were sent. A batch is only built while consecutive
    /// datagrams share a destination and the leading segment size; the caller
    /// keeps calling until the queue is empty. Falls back to one send per
    /// datagram when the kernel has no GSO.
    ///
    /// # Errors
    ///
    /// Any send failure other than `WouldBlock`, which is retried.
    pub async fn send_batch(&self, datagrams: &[Datagram]) -> io::Result<usize> {
        let Some(first) = datagrams.first() else {
            return Ok(0);
        };
        let max_segments = self.state.max_gso_segments().max(1);
        if max_segments <= 1 {
            self.send(first).await?;
            return Ok(1);
        }

        // GSO requires every segment but the last to be exactly `segment_size`
        // bytes, so the run ends at the first datagram that is a different
        // size, a different destination, or larger than the leader.
        let segment_size = first.payload.len();
        let mut run = 1;
        while run < datagrams.len().min(max_segments) {
            let Some(next) = datagrams.get(run) else {
                break;
            };
            let Some(previous) = datagrams.get(run - 1) else {
                break;
            };
            if next.peer != first.peer
                || previous.payload.len() != segment_size
                || next.payload.len() > segment_size
            {
                break;
            }
            run += 1;
        }

        if run == 1 {
            self.send(first).await?;
            return Ok(1);
        }

        let mut contents = Vec::with_capacity(segment_size * run);
        for datagram in datagrams.iter().take(run) {
            contents.extend_from_slice(&datagram.payload);
        }

        loop {
            self.socket.writable().await?;
            let transmit = Transmit {
                destination: first.peer,
                ecn: None,
                contents: &contents,
                segment_size: Some(segment_size),
                src_ip: None,
            };
            match self.socket.try_io(tokio::io::Interest::WRITABLE, || {
                self.state
                    .try_send(UdpSockRef::from(&self.socket), &transmit)
            }) {
                Ok(()) => return Ok(run),
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => continue,
                Err(err) => return Err(err),
            }
        }
    }
}

impl std::fmt::Debug for UdpTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UdpTransport")
            .field("local_addr", &self.local_addr)
            .field("max_gso_segments", &self.state.max_gso_segments())
            .finish()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_bound_socket_round_trips_a_datagram() {
        let mut server = UdpTransport::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let client = UdpTransport::bind("127.0.0.1:0".parse().unwrap()).unwrap();

        client
            .send(&Datagram::udp(
                client.local_addr(),
                server.local_addr(),
                &b"ping"[..],
            ))
            .await
            .unwrap();

        let mut out = Vec::new();
        let count = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            server.recv_batch(&mut out),
        )
        .await
        .unwrap()
        .unwrap();

        assert_eq!(count, 1);
        assert_eq!(&out[0].payload[..], b"ping");
        assert_eq!(out[0].peer, client.local_addr());
    }

    #[tokio::test]
    async fn a_uniform_run_is_sent_as_one_batch_and_arrives_as_separate_datagrams() {
        let mut server = UdpTransport::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let client = UdpTransport::bind("127.0.0.1:0".parse().unwrap()).unwrap();

        let batch: Vec<Datagram> = (0..4u8)
            .map(|i| Datagram::udp(client.local_addr(), server.local_addr(), &[i; 64][..]))
            .collect();

        let mut offset = 0;
        while offset < batch.len() {
            offset += client.send_batch(&batch[offset..]).await.unwrap();
        }

        let mut out = Vec::new();
        while out.len() < 4 {
            tokio::time::timeout(
                std::time::Duration::from_secs(5),
                server.recv_batch(&mut out),
            )
            .await
            .unwrap()
            .unwrap();
        }
        assert_eq!(out.len(), 4);
        for (i, dg) in out.iter().enumerate() {
            assert_eq!(dg.payload.len(), 64);
            assert_eq!(dg.payload[0], i as u8);
        }
    }

    #[tokio::test]
    async fn a_batch_to_two_destinations_stops_at_the_boundary() {
        let a = UdpTransport::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let b = UdpTransport::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let client = UdpTransport::bind("127.0.0.1:0".parse().unwrap()).unwrap();

        let batch = vec![
            Datagram::udp(client.local_addr(), a.local_addr(), &[1u8; 64][..]),
            Datagram::udp(client.local_addr(), b.local_addr(), &[2u8; 64][..]),
        ];
        // Whatever the kernel supports, the first call may not send the second
        // datagram, because it goes somewhere else.
        let sent = client.send_batch(&batch).await.unwrap();
        assert_eq!(sent, 1);
    }

    #[tokio::test]
    async fn an_empty_batch_sends_nothing() {
        let client = UdpTransport::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        assert_eq!(client.send_batch(&[]).await.unwrap(), 0);
    }
}
