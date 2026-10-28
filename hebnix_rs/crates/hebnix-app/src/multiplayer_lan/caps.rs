//! Linux capabilities. Nothing needs any now: Workshop multiplayer runs over
//! tailcat, the spoofer's proxy uses an unprivileged port and the LAN chat
//! capture is gone, so the packages no longer setcap the binary. Older
//! installs may still carry CAP_NET_ADMIN, which is then used to remove the
//! nftables rules those versions added (firewall.rs); without it that
//! cleanup is a no-op.

use std::process::Command;

// per linux/capability.h
const CAP_NET_ADMIN: u32 = 12;

/// `setcap cap_net_admin+eip` on the binary only grants CAP_NET_ADMIN to
/// *this* process - it does not propagate to child processes spawned via
/// fork+exec (e.g. `ip`, `nft`), which normally start with a clean
/// capability set regardless of what the parent has.
///
/// Promotes CAP_NET_ADMIN into our own Inheritable set (a process can always
/// add one of its own Permitted caps to its own Inheritable set - self-
/// modification, no extra privilege needed) - a prerequisite for
/// `command_with_net_admin` below, which raises the capability into each
/// `ip`/`nft` child's own Ambient set individually rather than our own.
///
/// Deliberately does NOT raise our own process's Ambient set globally (an
/// earlier version of this did, and it broke every Heroic-launched game:
/// Ambient is inherited by *every* descendant indefinitely, and bubblewrap
/// - used by Steam Linux Runtime's sandboxing, several forks down from
/// Heroic - treats an unexpected inherited capability as a sandbox-escape
/// red flag and refuses to run at all ("Unexpected capabilities but not
/// setuid, old file caps config?", verified live). Call this once at
/// startup.
pub fn raise_net_admin_ambient() {
    use caps::{CapSet, Capability};
    // best-effort: silently no-ops if CAP_NET_ADMIN isn't in this process's
    // Permitted set at all yet (e.g. a plain `cargo run` dev binary with no
    // setcap applied), or if running as root (which needs no capability
    // dance in the first place).
    let _ = caps::raise(None, CapSet::Inheritable, Capability::CAP_NET_ADMIN);
}

/// builds a `Command` for `program` that raises CAP_NET_ADMIN into *this one
/// child's* own Ambient set (via a pre_exec hook, running after fork but
/// before exec) rather than our own process's - see raise_net_admin_ambient
/// above for why that distinction matters. Requires raise_net_admin_ambient
/// to have already promoted CAP_NET_ADMIN into our own Inheritable set,
/// since fork() duplicates the parent's full capability state (Permitted,
/// Inheritable, ...) into the child before this hook runs, and the Ambient
/// raise itself needs the capability in both Permitted and Inheritable.
pub(super) fn command_with_net_admin(program: &str) -> Command {
    command_with_caps(program, &[CAP_NET_ADMIN])
}

fn command_with_caps(program: impl AsRef<std::ffi::OsStr>, caps: &[u32]) -> Command {
    use std::os::unix::process::CommandExt;
    let mut cmd = Command::new(program);
    let caps = caps.to_vec();
    unsafe {
        cmd.pre_exec(move || {
            const PR_CAP_AMBIENT: libc::c_int = 47;
            const PR_CAP_AMBIENT_RAISE: libc::c_ulong = 2;
            for &cap in &caps {
                libc::prctl(PR_CAP_AMBIENT, PR_CAP_AMBIENT_RAISE, cap as libc::c_ulong, 0, 0);
            }
            Ok(())
        });
    }
    cmd
}
