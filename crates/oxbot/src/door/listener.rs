//! The door's TCP + TLS listener, as an [`axum::serve::Listener`].
//!
//! - Only devices with a local (private) address get past `accept`.
//! - At most [`MAX_CONNECTIONS`] connections at once, handshakes included,
//!   and [`MAX_PER_PEER`] from any one address, so one device can't take
//!   every slot. A handshake has [`HANDSHAKE_TIMEOUT`] to finish.
//! - A connection that sends nothing for [`IDLE_TIMEOUT`] is closed. Phones
//!   ping every 15 s, so this is how the home computer notices one that walked away.
//! - It stops (and frees the port) when its stop signal fires or drops.
//! - Each connection counts the bytes it carried ([`ConnBytes`]), for
//!   Settings → Activity, and knows the address it came from.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, watch};
use tokio::time::{Instant, Sleep};
use tokio_rustls::TlsAcceptor;
use tokio_rustls::server::TlsStream;

/// Open connections allowed at once.
pub const MAX_CONNECTIONS: usize = 64;
/// Open connections allowed at once from one address.
pub const MAX_PER_PEER: usize = 8;
/// How long a TLS handshake may take.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a connection may stay silent.
pub const IDLE_TIMEOUT: Duration = Duration::from_mins(1);

/// Accepts connections in the background and hands finished TLS ones to the
/// web server.
pub struct TlsListener {
    ready: mpsc::Receiver<(Conn, SocketAddr)>,
    addr: SocketAddr,
}

impl TlsListener {
    /// Starts accepting on `tcp` until `stop` turns `true` or is dropped.
    ///
    /// # Errors
    /// Fails if `tcp` has no address.
    pub fn start(
        tcp: TcpListener,
        tls: TlsAcceptor,
        mut stop: watch::Receiver<bool>,
    ) -> std::io::Result<Self> {
        let addr = tcp.local_addr()?;
        let (tx, ready) = mpsc::channel(16);
        let slots = Arc::new(Semaphore::new(MAX_CONNECTIONS));
        let per_peer = PeerSlots::default();
        tokio::spawn(async move {
            loop {
                let accepted = tokio::select! {
                    accepted = tcp.accept() => accepted,
                    changed = stop.changed() => {
                        if changed.is_err() || *stop.borrow() {
                            break;
                        }
                        continue;
                    }
                };
                let Ok((stream, peer)) = accepted else {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                };
                if !super::network::is_private_peer(peer.ip()) {
                    tracing::warn!(%peer, "refused a device door connection from outside the home network");
                    continue;
                }
                let Some(own_slot) = per_peer.take(peer.ip()) else {
                    tracing::warn!(%peer, "too many device door connections from one address; refused one");
                    continue;
                };
                let Ok(permit) = Arc::clone(&slots).try_acquire_owned() else {
                    tracing::warn!(%peer, "too many device door connections; refused one");
                    continue;
                };
                let tls = tls.clone();
                let tx = tx.clone();
                tokio::spawn(async move {
                    match tokio::time::timeout(HANDSHAKE_TIMEOUT, tls.accept(stream)).await {
                        Ok(Ok(stream)) => {
                            let conn = Conn::new(stream, (permit, own_slot), peer.ip());
                            let _ = tx.send((conn, peer)).await;
                        }
                        Ok(Err(err)) => tracing::debug!(%peer, %err, "a TLS handshake failed"),
                        Err(_) => tracing::debug!(%peer, "a TLS handshake took too long"),
                    }
                });
            }
            tracing::debug!(%addr, "the device door stopped listening");
        });
        Ok(Self { ready, addr })
    }
}

impl axum::serve::Listener for TlsListener {
    type Io = Conn;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        match self.ready.recv().await {
            Some(conn) => conn,
            // Stopped: the server's own shutdown ends it.
            None => std::future::pending().await,
        }
    }

    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        Ok(self.addr)
    }
}

/// Open connections by address, for [`MAX_PER_PEER`].
#[derive(Debug, Clone, Default)]
struct PeerSlots(Arc<std::sync::Mutex<HashMap<IpAddr, usize>>>);

impl PeerSlots {
    /// One of `ip`'s slots, if it has one free; given back when dropped.
    fn take(&self, ip: IpAddr) -> Option<PeerSlot> {
        let mut open = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let count = open.entry(ip).or_default();
        if *count >= MAX_PER_PEER {
            return None;
        }
        *count += 1;
        Some(PeerSlot {
            slots: self.clone(),
            ip,
        })
    }
}

/// One address's open connection (see [`PeerSlots`]).
#[derive(Debug)]
struct PeerSlot {
    slots: PeerSlots,
    ip: IpAddr,
}

impl Drop for PeerSlot {
    fn drop(&mut self) {
        let mut open = self
            .slots
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(count) = open.get_mut(&self.ip) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                open.remove(&self.ip);
            }
        }
    }
}

/// The bytes one connection carried so far (inside TLS), readable while
/// it's open, and the address it came from. Handlers get it as
/// `ConnectInfo<ConnBytes>`.
#[derive(Debug, Clone, Default)]
pub struct ConnBytes {
    counts: Arc<[AtomicU64; 2]>,
    peer: Option<IpAddr>,
}

impl ConnBytes {
    /// Bytes read from the device.
    #[must_use]
    pub fn read(&self) -> u64 {
        self.counts[0].load(Ordering::Relaxed)
    }

    /// Bytes written to the device.
    #[must_use]
    pub fn written(&self) -> u64 {
        self.counts[1].load(Ordering::Relaxed)
    }

    /// The device's address, if known.
    #[must_use]
    pub fn peer(&self) -> Option<IpAddr> {
        self.peer
    }

    fn add_read(&self, n: usize) {
        self.counts[0].fetch_add(n as u64, Ordering::Relaxed);
    }

    fn add_written(&self, n: usize) {
        self.counts[1].fetch_add(n as u64, Ordering::Relaxed);
    }
}

impl axum::extract::connect_info::Connected<axum::serve::IncomingStream<'_, TlsListener>>
    for ConnBytes
{
    fn connect_info(stream: axum::serve::IncomingStream<'_, TlsListener>) -> Self {
        stream.io().bytes.clone()
    }
}

/// One device connection: closes after [`IDLE_TIMEOUT`] without anything
/// from the device, and frees its slots when dropped.
pub struct Conn {
    inner: TlsStream<TcpStream>,
    idle: Pin<Box<Sleep>>,
    bytes: ConnBytes,
    _slots: (OwnedSemaphorePermit, PeerSlot),
}

impl Conn {
    fn new(
        inner: TlsStream<TcpStream>,
        slots: (OwnedSemaphorePermit, PeerSlot),
        peer: IpAddr,
    ) -> Self {
        Self {
            inner,
            idle: Box::pin(tokio::time::sleep(IDLE_TIMEOUT)),
            bytes: ConnBytes {
                counts: Arc::default(),
                peer: Some(peer),
            },
            _slots: slots,
        }
    }
}

impl AsyncRead for Conn {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        let before = buf.filled().len();
        match Pin::new(&mut this.inner).poll_read(cx, buf) {
            Poll::Ready(Ok(())) => {
                if buf.filled().len() > before {
                    this.bytes.add_read(buf.filled().len() - before);
                    this.idle.as_mut().reset(Instant::now() + IDLE_TIMEOUT);
                }
                Poll::Ready(Ok(()))
            }
            Poll::Pending => {
                if this.idle.as_mut().poll(cx).is_ready() {
                    Poll::Ready(Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "the device went quiet",
                    )))
                } else {
                    Poll::Pending
                }
            }
            failed @ Poll::Ready(Err(_)) => failed,
        }
    }
}

impl AsyncWrite for Conn {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        let written = Pin::new(&mut this.inner).poll_write(cx, buf);
        if let Poll::Ready(Ok(n)) = written {
            this.bytes.add_written(n);
        }
        written
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        let written = Pin::new(&mut this.inner).poll_write_vectored(cx, bufs);
        if let Poll::Ready(Ok(n)) = written {
            this.bytes.add_written(n);
        }
        written
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_address_gets_only_its_share_of_connections() {
        let slots = PeerSlots::default();
        let phone: IpAddr = [192, 168, 1, 20].into();
        let other: IpAddr = [192, 168, 1, 21].into();
        let mut held: Vec<_> = (0..MAX_PER_PEER)
            .map(|_| slots.take(phone).unwrap())
            .collect();
        assert!(slots.take(phone).is_none(), "one too many");
        assert!(slots.take(other).is_some(), "others still get in");
        // A closed connection frees its slot.
        held.pop();
        assert!(slots.take(phone).is_some());
        drop(held);
        assert!(
            slots.0.lock().unwrap().get(&phone).is_none_or(|n| *n <= 1),
            "given back"
        );
    }
}
