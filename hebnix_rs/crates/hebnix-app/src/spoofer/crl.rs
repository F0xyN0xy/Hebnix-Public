//! tiny http listener on the crl port, hands out the empty crl. RL asks for it
//! mid handshake, without it the handshake just dies.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crossbeam_channel::Sender;

use crate::messages::AppMsg;
use crate::spoofer::ca::CRL_PORTS;

pub struct CrlServer {
    running: Arc<AtomicBool>,
    port: u16,
    worker: std::sync::Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl CrlServer {
    pub fn start(crl_der: Vec<u8>, tx: Sender<AppMsg>) -> Result<Self, String> {
        let listener = super::bind_local("CRL server", CRL_PORTS)?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        crate::spoofer::ca::set_crl_port(port);
        let running = Arc::new(AtomicBool::new(true));
        let crl = Arc::new(crl_der);

        let worker = {
            let running = Arc::clone(&running);
            std::thread::Builder::new()
                .name("spoofer-crl".into())
                .spawn(move || {
                    for stream in listener.incoming() {
                        if !running.load(Ordering::Relaxed) {
                            break;
                        }
                        if let Ok(s) = stream {
                            handle(s, &crl, &tx);
                        }
                    }
                })
                .map_err(|e| format!("cant spawn crl thread: {e}"))?
        };

        Ok(Self {
            running,
            port,
            worker: std::sync::Mutex::new(Some(worker)),
        })
    }

    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
        let _ = TcpStream::connect(format!("127.0.0.1:{}", self.port));
        if let Ok(mut worker) = self.worker.lock() {
            if let Some(worker) = worker.take() {
                let _ = worker.join();
            }
        }
    }
}

fn handle(mut stream: TcpStream, crl: &[u8], tx: &Sender<AppMsg>) {
    let timeout = Some(std::time::Duration::from_secs(2));
    let _ = stream.set_read_timeout(timeout);
    let _ = stream.set_write_timeout(timeout);
    let mut buf = [0u8; 1024];
    let _ = stream.read(&mut buf);
    let _ = tx.send(AppMsg::Log(format!(
        "[Spoofer] served CRL ({} bytes) for revocation check",
        crl.len()
    )));

    let header = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/pkix-crl\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        crl.len()
    );
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(crl);
    let _ = stream.flush();
}
