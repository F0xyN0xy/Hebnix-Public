//! Account-only reverse HTTPS proxy. The account hostname is redirected to this
//! loopback listener; Windows proxy settings and WebSocket hosts are not involved.
use super::{ca::Ca, proxy, rules::Rule};
use crate::messages::AppMsg;
use crossbeam_channel::Sender;
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use rustls::{ServerConnection, StreamOwned};
use std::net::{TcpListener, TcpStream};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

pub const ACCOUNT_HOST: &str = "api.epicgames.dev";
// A hosts redirect only changes the IP, so HTTPS must still listen on port 443.
// Dynamic port fallback is only suitable for the CRL URL, which includes its port.
const LISTEN_ADDR: &str = "127.0.0.1:443";

#[derive(Debug)]
struct AccountCertificate(proxy::CertResolver);
impl ResolvesServerCert for AccountCertificate {
    fn resolve(&self, hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        if hello
            .server_name()
            .is_some_and(|name| !name.eq_ignore_ascii_case(ACCOUNT_HOST))
        {
            return None;
        }
        self.0.resolve(hello)
    }
}

pub struct HttpProxy {
    running: Arc<AtomicBool>,
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
}
impl HttpProxy {
    pub fn start(
        ca: Arc<Ca>,
        rules: Arc<Vec<Box<dyn Rule>>>,
        tx: Sender<AppMsg>,
    ) -> Result<Self, String> {
        // Resolve before publishing the redirect and pin upstream DNS independently of hosts.
        let ip = super::dns::resolve_a(ACCOUNT_HOST)?;
        let upstream = proxy::build_upstream_agent_pins([(ACCOUNT_HOST.to_owned(), ip)].into());
        let config = proxy::build_server_config(Arc::new(AccountCertificate(
            proxy::CertResolver::with_default_host(ca, ACCOUNT_HOST),
        )))?;
        let listener = TcpListener::bind(LISTEN_ADDR)
            .map_err(|e| format!("Cannot start account HTTPS proxy on {LISTEN_ADDR}: {e}"))?;
        let running = Arc::new(AtomicBool::new(true));
        let active = Arc::clone(&running);
        let reported = Arc::new(AtomicBool::new(false));
        let worker = std::thread::Builder::new().name("account-https-proxy".into()).spawn(move || {
            for stream in listener.incoming() {
                if !active.load(Ordering::Acquire) { break; }
                let Ok(stream) = stream else { continue; };
                let config = Arc::clone(&config); let upstream = upstream.clone();
                let rules = Arc::clone(&rules); let tx = tx.clone(); let reported = Arc::clone(&reported);
                std::thread::spawn(move || {
                    if handle(stream,config,upstream,rules,tx.clone()).is_err() && !reported.swap(true,Ordering::Relaxed) {
                        // Never record account response bodies, authentication headers or URLs.
                        let _ = tx.send(AppMsg::Log("[Spoofer] An account HTTPS connection failed. Check the Hebnix certificate and restart Rocket League.".into()));
                    }
                });
            }
        }).map_err(|e|format!("Cannot start account proxy thread: {e}"))?;
        Ok(Self {
            running,
            worker: Mutex::new(Some(worker)),
        })
    }
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire)
            && self
                .worker
                .lock()
                .is_ok_and(|w| w.as_ref().is_some_and(|w| !w.is_finished()))
    }
    pub fn stop(&self) {
        if !self.running.swap(false, Ordering::AcqRel) {
            return;
        }
        let _ = TcpStream::connect(LISTEN_ADDR);
        if let Ok(mut worker) = self.worker.lock() {
            if let Some(worker) = worker.take() {
                let _ = worker.join();
            }
        }
    }
}
impl Drop for HttpProxy {
    fn drop(&mut self) {
        self.stop();
    }
}

fn handle(
    client: TcpStream,
    config: Arc<rustls::ServerConfig>,
    upstream: ureq::Agent,
    rules: Arc<Vec<Box<dyn Rule>>>,
    tx: Sender<AppMsg>,
) -> Result<(), String> {
    let timeout = Some(std::time::Duration::from_secs(15));
    client
        .set_read_timeout(timeout)
        .map_err(|e| e.to_string())?;
    client
        .set_write_timeout(timeout)
        .map_err(|e| e.to_string())?;
    let mut connection = ServerConnection::new(config).map_err(|e| e.to_string())?;
    let mut client = client;
    while connection.is_handshaking() {
        connection
            .complete_io(&mut client)
            .map_err(|e| e.to_string())?;
    }
    if connection
        .server_name()
        .is_some_and(|host| !host.eq_ignore_ascii_case(ACCOUNT_HOST))
    {
        return Err("Host is outside the account proxy scope".into());
    }
    proxy::serve_one(
        StreamOwned::new(connection, client),
        &upstream,
        &rules,
        &tx,
        ACCOUNT_HOST,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    // Use a temporary loopback port and in-memory certificates: these tests never
    // modify the certificate store, hosts file, or Windows proxy settings.
    fn tls_request(host: &str) -> Result<(), String> {
        let key = rcgen::KeyPair::generate().unwrap();
        let cert = rcgen::CertificateParams::new(vec![host.to_owned()])
            .unwrap()
            .self_signed(&key)
            .unwrap();
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let server_config = rustls::ServerConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![cert.der().clone()],
                rustls::pki_types::PrivatePkcs8KeyDer::from(key.serialize_der()).into(),
            )
            .unwrap();
        let mut roots = rustls::RootCertStore::empty();
        roots.add(cert.der().clone()).unwrap();
        let client_config = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let socket = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        socket
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        socket
            .set_write_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let worker = std::thread::spawn(move || {
            handle(
                listener.accept().unwrap().0,
                Arc::new(server_config),
                proxy::build_upstream_agent_pins(Default::default()),
                Arc::new(Vec::new()),
                crossbeam_channel::unbounded().0,
            )
        });
        let connection = rustls::ClientConnection::new(
            Arc::new(client_config),
            host.to_owned().try_into().unwrap(),
        )
        .unwrap();
        let mut client = StreamOwned::new(connection, socket);
        // Invalid HTTP reaches the parser after TLS, without making an upstream request.
        let _ = client.write_all(b"INVALID\r\n\r\n");
        let _ = client.flush();
        // Keep the socket alive while the server reads: closing with unread TLS
        // session tickets can reset the connection on Windows.
        let result = worker.join().unwrap();
        drop(client);
        result
    }

    #[test]
    fn account_tls_handshake_reaches_http_parser() {
        let error = tls_request(ACCOUNT_HOST).unwrap_err();
        assert!(error.starts_with("parse req:"), "{error}");
    }

    #[test]
    fn unrelated_tls_hosts_are_rejected_before_http_forwarding() {
        let error = tls_request("ws.psynet.gg").unwrap_err();
        assert_eq!(error, "Host is outside the account proxy scope");
    }
}
