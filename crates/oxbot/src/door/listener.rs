//! The door's TCP + TLS listener, as an [`axum::serve::Listener`].
//!
//! - Only devices with a local (private) address get past `accept`.
//! - At most [`MAX_CONNECTIONS`] connections at once, handshakes included,
//!   and a handshake has [`HANDSHAKE_TIMEOUT`] to finish.
//! - A connection that sends nothing for [`IDLE_TIMEOUT`] is closed. Phones
//!   ping every 15 s, so this is how the Farm notices one that walked away.
//! - It stops (and frees the port) when its stop signal fires or drops.

use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
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
                    tracing::warn!(%peer, "refused a Gate door connection from outside the home network");
                    continue;
                }
                let Ok(permit) = Arc::clone(&slots).try_acquire_owned() else {
                    tracing::warn!(%peer, "too many Gate door connections; refused one");
                    continue;
                };
                let tls = tls.clone();
                let tx = tx.clone();
                tokio::spawn(async move {
                    match tokio::time::timeout(HANDSHAKE_TIMEOUT, tls.accept(stream)).await {
                        Ok(Ok(stream)) => {
                            let _ = tx.send((Conn::new(stream, permit), peer)).await;
                        }
                        Ok(Err(err)) => tracing::debug!(%peer, %err, "a TLS handshake failed"),
                        Err(_) => tracing::debug!(%peer, "a TLS handshake took too long"),
                    }
                });
            }
            tracing::debug!(%addr, "the Gate door stopped listening");
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

/// One Gate connection: closes after [`IDLE_TIMEOUT`] without anything
/// from the Gate, and frees its slot when dropped.
pub struct Conn {
    inner: TlsStream<TcpStream>,
    idle: Pin<Box<Sleep>>,
    _slot: OwnedSemaphorePermit,
}

impl Conn {
    fn new(inner: TlsStream<TcpStream>, slot: OwnedSemaphorePermit) -> Self {
        Self {
            inner,
            idle: Box::pin(tokio::time::sleep(IDLE_TIMEOUT)),
            _slot: slot,
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
        Pin::new(&mut self.get_mut().inner).poll_write(cx, buf)
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
        Pin::new(&mut self.get_mut().inner).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }
}
