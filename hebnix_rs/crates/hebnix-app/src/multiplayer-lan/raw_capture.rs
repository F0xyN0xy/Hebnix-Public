// A WinDivert handle in sniff mode: copies IP packets matching a filter
// without diverting or changing them (needs Hebnix running as
// administrator). chat_capture.rs reads Rocket League's LAN chat out of it.
// (It used to also catch RL's LAN discovery broadcast for the old tailnet
// beacon relay; the tailcat sidecar shares RL's discovery port instead.)

use std::ffi::{CString, c_void};
use std::net::{Ipv4Addr, SocketAddr};

use libloading::Library;
use windows::Win32::Foundation::HANDLE;
use windows::core::BOOL;

const WINDIVERT_LAYER_NETWORK: u32 = 0;
pub(super) const WINDIVERT_FLAG_SNIFF: u64 = 0x0001;
pub(super) const WINDIVERT_FLAG_RECV_ONLY: u64 = 0x0004;
const WINDIVERT_SHUTDOWN_BOTH: u32 = 3;

/// Thin, self-owned wrapper around the raw WinDivertOpen/Recv/Shutdown/Close
/// calls -- deliberately NOT using the `windivert` crate's own `WinDivert`
/// type for this handle. That wrapper requires `&mut self` to call
/// `shutdown()`/`close()`, which makes it impossible to cancel a `recv()`
/// that's blocked forever on another thread without fighting the borrow
/// checker. The underlying WinDivert C API is explicit that
/// WinDivertShutdown() is safe to call on the SAME handle from a different
/// thread than the one blocked in WinDivertRecv() - that's its documented
/// purpose. Going around the wrapper crate to call these directly lets the
/// capture thread be shut down cleanly and directly, instead of the old
/// approach of stopping/uninstalling the whole WinDivert *service* to force
/// the blocked read to unblock as a side effect - which left the service in
/// a genuinely corrupted state (disabled, marked for deletion, stuck
/// stop-pending) after repeated start/stop cycles. The service itself is
/// now never touched at shutdown at all.
pub(super) struct RawCapture {
    handle: HANDLE,
    // Keep the DLL mapped for every call and until after WinDivertClose().
    _library: Library,
    recv: unsafe extern "C" fn(HANDLE, *mut c_void, u32, *mut u32, *mut c_void) -> BOOL,
    shutdown: unsafe extern "C" fn(HANDLE, u32) -> BOOL,
    close: unsafe extern "C" fn(HANDLE) -> BOOL,
}

// HANDLE is just a wrapper around an isize - safe to share and use
// concurrently from multiple threads, which is exactly what WinDivert's own
// API contract for WinDivertShutdown() requires.
unsafe impl Send for RawCapture {}
unsafe impl Sync for RawCapture {}

impl RawCapture {
    /// Loads WinDivert only once Hebnix is elevated and the runtime bundle has
    /// been extracted to its AppData folder. This avoids an eager process-load
    /// dependency on a DLL/driver that may not exist yet.
    pub(super) fn open(filter: &str, flags: u64) -> Result<Self, String> {
        if !crate::spoofer::is_admin() {
            return Err("WinDivert capture requires Hebnix to run as administrator".to_string());
        }

        let base_dir = crate::config::base_dir();
        crate::multiplayer_assets::ensure_present(&base_dir)
            .map_err(|error| format!("could not extract multiplayer components: {error}"))?;
        let dll_path = base_dir.join("multiplayer-lan").join("WinDivert.dll");
        let library = unsafe { Library::new(&dll_path) }
            .map_err(|error| format!("could not load {}: {error}", dll_path.display()))?;

        // Function pointers are copied out while `library` remains owned by
        // RawCapture, so the DLL cannot unload before the final Close call.
        let open = unsafe {
            *library
                .get::<unsafe extern "C" fn(*const i8, u32, i16, u64) -> HANDLE>(b"WinDivertOpen\0")
                .map_err(|error| format!("could not find WinDivertOpen: {error}"))?
        };
        let recv = unsafe {
            *library
                .get::<unsafe extern "C" fn(HANDLE, *mut c_void, u32, *mut u32, *mut c_void) -> BOOL>(b"WinDivertRecv\0")
                .map_err(|error| format!("could not find WinDivertRecv: {error}"))?
        };
        let shutdown = unsafe {
            *library
                .get::<unsafe extern "C" fn(HANDLE, u32) -> BOOL>(b"WinDivertShutdown\0")
                .map_err(|error| format!("could not find WinDivertShutdown: {error}"))?
        };
        let close = unsafe {
            *library
                .get::<unsafe extern "C" fn(HANDLE) -> BOOL>(b"WinDivertClose\0")
                .map_err(|error| format!("could not find WinDivertClose: {error}"))?
        };

        let filter = CString::new(filter).map_err(|error| error.to_string())?;
        let handle = unsafe { open(filter.as_ptr(), WINDIVERT_LAYER_NETWORK, 0, flags) };
        if handle.is_invalid() {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(Self {
            handle,
            _library: library,
            recv,
            shutdown,
            close,
        })
    }

    pub(super) fn recv(&self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let mut recv_len: u32 = 0;
        let ok = unsafe {
            (self.recv)(
                self.handle,
                buffer.as_mut_ptr() as *mut _,
                buffer.len() as u32,
                &mut recv_len,
                std::ptr::null_mut(),
            )
        };
        if ok.as_bool() {
            Ok(recv_len as usize)
        } else {
            Err(std::io::Error::last_os_error())
        }
    }

    /// unblocks a `recv()` call in progress on another thread - see the
    /// struct doc comment for why this is safe to call concurrently
    pub(super) fn shutdown(&self) {
        unsafe {
            let _ = (self.shutdown)(self.handle, WINDIVERT_SHUTDOWN_BOTH);
        }
    }
}

impl Drop for RawCapture {
    fn drop(&mut self) {
        unsafe {
            let _ = (self.close)(self.handle);
        }
    }
}

/// pulls the UDP payload, source address, and destination port out of a raw
/// IP packet - WinDivert's network layer hands us the packet starting at
/// the IP header (no Ethernet header, it's already above that), so this is
/// the same job a normal socket's recv_from would otherwise do
pub(super) fn parse_udp_payload(ip: &[u8]) -> Option<(Vec<u8>, SocketAddr, u16)> {
    const UDP_PROTOCOL: u8 = 17;

    if ip.len() < 20 {
        return None;
    }
    let version = ip[0] >> 4;
    if version != 4 {
        return None;
    }
    let header_len = (ip[0] & 0x0F) as usize * 4;
    if header_len < 20 || ip.len() < header_len + 8 {
        return None;
    }
    if ip[9] != UDP_PROTOCOL {
        return None;
    }
    let source_ip = Ipv4Addr::new(ip[12], ip[13], ip[14], ip[15]);

    let udp = &ip[header_len..];
    let source_port = u16::from_be_bytes([udp[0], udp[1]]);
    let destination_port = u16::from_be_bytes([udp[2], udp[3]]);
    let udp_length = u16::from_be_bytes([udp[4], udp[5]]) as usize;
    if udp_length < 8 || udp.len() < 8 {
        return None;
    }
    let payload_end = udp_length.min(udp.len());
    let payload = udp[8..payload_end].to_vec();

    Some((
        payload,
        SocketAddr::new(source_ip.into(), source_port),
        destination_port,
    ))
}
