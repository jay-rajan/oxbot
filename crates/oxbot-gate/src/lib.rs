//! The Gate side of the link to the Farm.
//!
//! A Gate (a phone, or a laptop in Gate mode) pairs once by reading the
//! Farm's pairing link (the QR code), then connects to the Farm's Gate door
//! over TLS. The Farm made its own certificate, so instead of trusting
//! certificate authorities the Gate checks that the certificate's SHA-256
//! matches the fingerprint in the pairing link ("pinning"). Nothing goes
//! anywhere but the Farm.
//!
//! - [`PairLink::parse`] reads `oxbot://pair?h=…&f=…&c=…&n=…`, and
//!   [`PairLink::to_link`] writes it (the Farm uses this crate for both, so
//!   the format has one owner).
//! - [`pair`] sends the code and gets this Gate's key: a [`Paired`] to keep.
//! - [`fetch`] gets a page from the Farm; [`connect`] opens the WebSocket.
//! - [`copy::ChatCopy`] keeps the phone's own copy of its chats, for
//!   reading away from home.

pub mod copy;

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt as _, Full, Limited};
use oxbot_protocol::{PairReply, PairRequest};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use serde::{Deserialize, Serialize};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

/// The name the Farm's certificate is made for.
pub const CERT_NAME: &str = "oxbot-farm";
/// How long the Farm has to answer a request.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(30);
/// How long to wait for one address before trying the next.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(4);
/// The most a page may be.
const MAX_BODY: usize = 8 * 1024 * 1024;

/// What went wrong talking to the Farm.
#[derive(Debug, thiserror::Error)]
pub enum GateError {
    /// The scanned or pasted link isn't an oxbot pairing link.
    #[error("That isn't an oxbot pairing code. Scan the QR code in the Farm's Settings → Gates.")]
    BadLink,
    /// None of the Farm's addresses answered.
    #[error(
        "Can't reach the Farm. Check that this phone is on the same Wi-Fi as the Mac and oxbot is running there."
    )]
    Unreachable,
    /// Something answered, but its certificate isn't the Farm's.
    #[error(
        "Something on this Wi-Fi answered as the Farm but isn't it (its certificate doesn't match), so oxbot didn't connect."
    )]
    WrongFarm,
    /// The Farm doesn't know this Gate's key (it was removed).
    #[error("The Farm doesn't know this phone any more (it was removed). Pair it again.")]
    Unpaired,
    /// The Farm couldn't answer just now (it's still paired); try again.
    #[error("{0}")]
    Busy(String),
    /// The Farm said no, with its reason.
    #[error("{0}")]
    Refused(String),
    /// The Farm's answer made no sense.
    #[error("The Farm's answer couldn't be read: {0}")]
    BadReply(String),
    /// The WebSocket failed.
    #[error("connection: {0}")]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),
}

/// Where the Farm is and how to recognize it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Farm {
    /// Addresses to try, best first, e.g. `192.168.1.20:7879`.
    pub hosts: Vec<String>,
    /// SHA-256 of the Farm's certificate, lowercase hex.
    pub fingerprint: String,
    /// The Farm's name, e.g. `Jay's Mac mini`.
    pub name: String,
}

/// A pairing link, read from the QR code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairLink {
    /// The Farm.
    pub farm: Farm,
    /// The one-time code.
    pub code: String,
}

impl PairLink {
    /// Reads `oxbot://pair?h=<host:port>&h=…&f=<fingerprint>&c=<code>&n=<name>`.
    ///
    /// # Errors
    /// [`GateError::BadLink`] if anything is missing.
    pub fn parse(link: &str) -> Result<Self, GateError> {
        let url = url::Url::parse(link.trim()).map_err(|_| GateError::BadLink)?;
        if url.scheme() != "oxbot" || url.host_str() != Some("pair") {
            return Err(GateError::BadLink);
        }
        let mut hosts = Vec::new();
        let (mut fingerprint, mut code, mut name) = (None, None, None);
        for (k, v) in url.query_pairs() {
            match k.as_ref() {
                "h" if !v.is_empty() => hosts.push(v.into_owned()),
                "f" => fingerprint = Some(v.to_ascii_lowercase()),
                "c" => code = Some(v.into_owned()),
                "n" => name = Some(v.into_owned()),
                _ => {}
            }
        }
        let fingerprint = fingerprint
            .filter(|f| f.len() == 64 && f.bytes().all(|b| b.is_ascii_hexdigit()))
            .ok_or(GateError::BadLink)?;
        let code = code.filter(|c| !c.is_empty()).ok_or(GateError::BadLink)?;
        if hosts.is_empty() {
            return Err(GateError::BadLink);
        }
        Ok(Self {
            farm: Farm {
                hosts,
                fingerprint,
                name: name.unwrap_or_else(|| "oxbot".to_owned()),
            },
            code,
        })
    }

    /// The link for the QR code:
    /// `oxbot://pair?h=<host:port>&h=…&f=<fingerprint>&c=<code>&n=<farm name>`.
    #[must_use]
    pub fn to_link(&self) -> String {
        let mut url = url::Url::parse("oxbot://pair").unwrap_or_else(|_| unreachable!());
        {
            let mut q = url.query_pairs_mut();
            for host in &self.farm.hosts {
                q.append_pair("h", host);
            }
            q.append_pair("f", &self.farm.fingerprint);
            q.append_pair("c", &self.code);
            q.append_pair("n", &self.farm.name);
        }
        url.to_string()
    }
}

/// A paired Gate: what it keeps to connect from now on. `key` is a secret:
/// keep it in the platform's secure storage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Paired {
    /// The Farm.
    pub farm: Farm,
    /// This Gate's id on the Farm.
    pub id: String,
    /// Who this Gate belongs to.
    pub person: String,
    /// This Gate's key.
    pub key: String,
}

/// Pairs with the Farm in `link`, calling this device `device`.
///
/// # Errors
/// The Farm can't be reached or isn't the one in the link, or the code is
/// wrong or ran out.
pub async fn pair(link: &PairLink, device: &str) -> Result<Paired, GateError> {
    let body = serde_json::to_vec(&PairRequest {
        code: link.code.clone(),
        device: device.to_owned(),
    })
    .map_err(|e| GateError::BadReply(e.to_string()))?;
    let (_, reply) = request(&link.farm, "POST", "/pair", None, Some(&body)).await?;
    let reply: PairReply =
        serde_json::from_slice(&reply).map_err(|e| GateError::BadReply(e.to_string()))?;
    Ok(Paired {
        farm: Farm {
            name: reply.farm,
            ..link.farm.clone()
        },
        id: reply.id,
        person: reply.person,
        key: reply.key,
    })
}

/// Gets `path` (e.g. `/`) from the Farm. Returns which address answered and
/// the body.
///
/// # Errors
/// The Farm can't be reached, isn't the right one, or doesn't know this Gate.
pub async fn fetch(paired: &Paired, path: &str) -> Result<(String, Vec<u8>), GateError> {
    request(&paired.farm, "GET", path, Some(&paired.key), None).await
}

/// A WebSocket message.
pub use tokio_tungstenite::tungstenite::Message;

/// A WebSocket to the Farm, through TLS.
pub type Socket = tokio_tungstenite::WebSocketStream<TlsStream<TcpStream>>;

/// Opens the WebSocket to the Farm. Returns which address answered.
///
/// # Errors
/// The Farm can't be reached, isn't the right one, or doesn't know this Gate.
pub async fn connect(paired: &Paired) -> Result<(String, Socket), GateError> {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
    use tokio_tungstenite::tungstenite::http::HeaderValue;
    let (host, tls) = open(&paired.farm).await?;
    let mut req = format!("ws://{CERT_NAME}/ws").into_client_request()?;
    let bearer = HeaderValue::from_str(&format!("Bearer {}", paired.key))
        .map_err(|e| GateError::BadReply(e.to_string()))?;
    req.headers_mut().insert("authorization", bearer);
    match tokio_tungstenite::client_async(req, tls).await {
        Ok((socket, _)) => Ok((host, socket)),
        Err(tokio_tungstenite::tungstenite::Error::Http(resp)) if resp.status().as_u16() == 401 => {
            Err(GateError::Unpaired)
        }
        Err(e) => Err(e.into()),
    }
}

/// One HTTP request over a fresh TLS connection.
async fn request(
    farm: &Farm,
    method: &str,
    path: &str,
    key: Option<&str>,
    body: Option<&[u8]>,
) -> Result<(String, Vec<u8>), GateError> {
    let bad = |e: &dyn std::fmt::Display| GateError::BadReply(e.to_string());
    let (host, tls) = open(farm).await?;
    let (mut sender, connection) =
        hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(tls))
            .await
            .map_err(|e| bad(&e))?;
    // Drives the connection; ends when the answer is read and `sender` drops.
    tokio::spawn(async move {
        if let Err(err) = connection.await {
            tracing::debug!(%err, "the link to the Farm closed");
        }
    });
    let mut req = hyper::Request::builder()
        .method(method)
        .uri(path)
        .header(hyper::header::HOST, CERT_NAME);
    if let Some(key) = key {
        req = req.header(hyper::header::AUTHORIZATION, format!("Bearer {key}"));
    }
    if body.is_some() {
        req = req.header(hyper::header::CONTENT_TYPE, "application/json");
    }
    let req = req
        .body(Full::new(Bytes::copy_from_slice(body.unwrap_or_default())))
        .map_err(|e| bad(&e))?;
    let answer = async {
        let resp = sender.send_request(req).await.map_err(|e| bad(&e))?;
        let status = resp.status().as_u16();
        // A cut-off answer is an error here, never a short page.
        let body = Limited::new(resp.into_body(), MAX_BODY)
            .collect()
            .await
            .map_err(|e| bad(&e))?
            .to_bytes()
            .to_vec();
        Ok::<_, GateError>((status, body))
    };
    let (status, body) = tokio::time::timeout(ANSWER_TIMEOUT, answer)
        .await
        .map_err(|_| bad(&"the Farm took too long to answer"))??;
    match status {
        200 => Ok((host, body)),
        401 => Err(GateError::Unpaired),
        503 => Err(GateError::Busy(
            String::from_utf8_lossy(&body).trim().to_owned(),
        )),
        _ => Err(GateError::Refused(
            String::from_utf8_lossy(&body).trim().to_owned(),
        )),
    }
}

/// Opens TLS to the first of the Farm's addresses that answers, checking the
/// certificate is the pinned one.
async fn open(farm: &Farm) -> Result<(String, TlsStream<TcpStream>), GateError> {
    let connector = connector(&farm.fingerprint)?;
    let name = ServerName::try_from(CERT_NAME).map_err(|e| GateError::BadReply(e.to_string()))?;
    let mut wrong_farm = false;
    for host in &farm.hosts {
        let Ok(Ok(tcp)) = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(host)).await
        else {
            tracing::debug!(host, "the Farm didn't answer here");
            continue;
        };
        let _ = tcp.set_nodelay(true);
        match tokio::time::timeout(CONNECT_TIMEOUT, connector.connect(name.clone(), tcp)).await {
            Ok(Ok(tls)) => return Ok((host.clone(), tls)),
            Ok(Err(err)) => {
                tracing::warn!(host, %err, "TLS to the Farm failed");
                wrong_farm |= err.to_string().contains("pinned");
            }
            Err(_) => tracing::debug!(host, "TLS to the Farm took too long"),
        }
    }
    Err(if wrong_farm {
        GateError::WrongFarm
    } else {
        GateError::Unreachable
    })
}

fn connector(fingerprint: &str) -> Result<TlsConnector, GateError> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = Arc::new(Pinned {
        fingerprint: fingerprint.to_ascii_lowercase(),
        provider: Arc::clone(&provider),
    });
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| GateError::BadReply(e.to_string()))?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    Ok(TlsConnector::from(Arc::new(config)))
}

/// SHA-256 of `bytes` as lowercase hex.
#[must_use]
pub fn fingerprint(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .fold(String::new(), |mut out, b| {
            let _ = write!(out, "{b:02x}");
            out
        })
}

/// Accepts exactly one certificate: the Farm's, by fingerprint. Signatures
/// are still checked, so only the holder of the Farm's key can answer.
#[derive(Debug)]
struct Pinned {
    fingerprint: String,
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl ServerCertVerifier for Pinned {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if fingerprint(end_entity.as_ref()) == self.fingerprint {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General(
                "not the pinned Farm certificate".to_owned(),
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FP: &str = "ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12";

    #[test]
    fn reads_a_pairing_link() {
        let link = format!(
            "oxbot://pair?h=192.168.1.20%3A7879&h=Jays-Mac.local%3A7879&f={}&c=K7QMR3XP&n=Jay%27s+Mac",
            FP.to_uppercase()
        );
        let link = PairLink::parse(&link).unwrap();
        assert_eq!(
            link.farm.hosts,
            ["192.168.1.20:7879", "Jays-Mac.local:7879"]
        );
        assert_eq!(link.farm.fingerprint, FP);
        assert_eq!(link.farm.name, "Jay's Mac");
        assert_eq!(link.code, "K7QMR3XP");
    }

    #[test]
    fn refuses_links_that_arent_oxbot_pairing_links() {
        for bad in [
            "https://example.com/pair?h=1.2.3.4:1&c=x",
            &format!("oxbot://pair?f={FP}&c=K7QM"),
            "oxbot://pair?h=1.2.3.4:7879&f=short&c=K7QM",
            &format!("oxbot://pair?h=1.2.3.4:7879&f={FP}"),
            "not a link",
        ] {
            assert!(PairLink::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_link_reads_back_as_it_was_written() {
        let link = PairLink {
            farm: Farm {
                hosts: vec!["192.168.1.20:7879".into(), "Jays-Mac.local:7879".into()],
                fingerprint: FP.into(),
                name: "Jay's Mac".into(),
            },
            code: "K7QMR3XP".into(),
        };
        assert_eq!(PairLink::parse(&link.to_link()).unwrap(), link);
    }
}
