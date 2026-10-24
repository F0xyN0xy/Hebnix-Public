// crates/hebnix-app/src/spoofer/socket.rs
//! socket proxy. reverse is 443 after a hosts redirect

use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use crossbeam_channel::Sender;
use rustls::{ServerConfig, ServerConnection, StreamOwned};

use crate::messages::AppMsg;
use crate::spoofer::ca::Ca;
use crate::spoofer::proxy::{
    CertResolver, build_server_config, build_upstream_agent_pins, serve_one,
};
use crate::spoofer::rules::Rule;

// The hosts redirect is explicitly 127.0.0.1, so match the C# relay and do
// not expose the interception endpoint on the local network.
pub const REVERSE_ADDR: &str = "127.0.0.1:443";

pub struct SocketProxy {
    running: Arc<AtomicBool>,
    addr: String,
    worker: std::sync::Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl SocketProxy {
    pub fn start(
        ca: Arc<Ca>,
        rules: Arc<Vec<Box<dyn Rule>>>,
        tx: Sender<AppMsg>,
        real_ips: std::collections::HashMap<String, std::net::Ipv4Addr>,
    ) -> Result<Self, String> {
        let addr = REVERSE_ADDR.to_string();
        let listener = TcpListener::bind(&addr).map_err(|e| {
            if e.kind() == std::io::ErrorKind::PermissionDenied
                || e.kind() == std::io::ErrorKind::AddrInUse
            {
                format!("cant bind {addr}, something else is using it: {e}")
            } else {
                format!("cant bind {addr}: {e}")
            }
        })?;
        let running = Arc::new(AtomicBool::new(true));

        let resolver = Arc::new(CertResolver::new(ca));
        let server_config = build_server_config(resolver)?;
        let upstream = build_upstream_agent_pins(real_ips);

        let worker = {
            let running = Arc::clone(&running);
            let addr_clone = addr.clone();
            std::thread::Builder::new()
                .name("spoofer-socket".into())
                .spawn(move || {
                    accept_loop(
                        listener,
                        &running,
                        &server_config,
                        &upstream,
                        &rules,
                        &tx,
                        &addr_clone,
                    )
                })
                .map_err(|e| format!("cant spawn socket proxy thread: {e}"))?
        };

        Ok(Self {
            running,
            addr,
            worker: std::sync::Mutex::new(Some(worker)),
        })
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire)
            && self
                .worker
                .lock()
                .map(|worker| worker.as_ref().is_some_and(|worker| !worker.is_finished()))
                .unwrap_or(false)
    }

    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
        let _ = TcpStream::connect(&self.addr);
        if let Ok(mut worker) = self.worker.lock() {
            if let Some(worker) = worker.take() {
                let _ = worker.join();
            }
        }
    }
}

fn accept_loop(
    listener: TcpListener,
    running: &AtomicBool,
    server_config: &Arc<ServerConfig>,
    upstream: &ureq::Agent,
    rules: &Arc<Vec<Box<dyn Rule>>>,
    tx: &Sender<AppMsg>,
    addr: &str,
) {
    tracing::info!("spoofer socket proxy listening on {addr}");
    let observed_hosts = Arc::new(AtomicU8::new(0));
    let error_reported = Arc::new(AtomicBool::new(false));
    for stream in listener.incoming() {
        if !running.load(Ordering::Relaxed) {
            break;
        }
        let Ok(stream) = stream else { continue };

        let server_config = Arc::clone(server_config);
        let upstream = upstream.clone();
        let rules = Arc::clone(rules);
        let tx = tx.clone();
        let observed_hosts = Arc::clone(&observed_hosts);
        let error_reported = Arc::clone(&error_reported);
        std::thread::spawn(move || {
            if let Err(e) = handle_reverse(
                stream,
                &server_config,
                &upstream,
                &rules,
                &tx,
                &observed_hosts,
            ) {
                tracing::debug!("socket proxy conn ended: {e}");
                if !error_reported.swap(true, Ordering::Relaxed) {
                    // Upstream errors can contain URLs and account identifiers.
                    // Keep the detailed error in debug logs, not the Console.
                    let _ = tx.send(AppMsg::Log(
                        "[Spoofer] An intercepted connection failed. See the debug log for details. Restart Rocket League after checking the proxy certificate and connection settings."
                            .into(),
                    ));
                }
            }
        });
    }
    tracing::info!("spoofer socket proxy stopped");
}

fn handle_reverse(
    client: TcpStream,
    server_config: &Arc<ServerConfig>,
    upstream: &ureq::Agent,
    rules: &Arc<Vec<Box<dyn Rule>>>,
    tx: &Sender<AppMsg>,
    observed_hosts: &AtomicU8,
) -> Result<(), String> {
    let mut conn =
        ServerConnection::new(Arc::clone(server_config)).map_err(|e| format!("tls conn: {e}"))?;
    let mut sock = client;
    let timeout = Some(std::time::Duration::from_secs(15));
    sock.set_read_timeout(timeout)
        .map_err(|e| format!("read timeout: {e}"))?;
    sock.set_write_timeout(timeout)
        .map_err(|e| format!("write timeout: {e}"))?;
    while conn.is_handshaking() {
        conn.complete_io(&mut sock)
            .map_err(|e| format!("tls handshake: {e}"))?;
    }
    let host = conn
        .server_name()
        .map(str::to_string)
        .ok_or("no sni on the reverse connection")?;
    tracing::debug!("reverse {host}");
    let Some(host_index) = ["api.epicgames.dev", "config.psynet.gg"]
        .iter()
        .position(|allowed| host.eq_ignore_ascii_case(allowed))
    else {
        return Err("Host is outside the Hebnix proxy scope".into());
    };
    let host_bit = 1 << host_index;
    if observed_hosts.fetch_or(host_bit, Ordering::Relaxed) & host_bit == 0 {
        let _ = tx.send(AppMsg::Log(format!(
            "[Spoofer] Intercepted TLS connection to {host}"
        )));
    }

    let tls = StreamOwned::new(conn, sock);
    serve_one(tls, upstream, rules, tx, &host)
}
