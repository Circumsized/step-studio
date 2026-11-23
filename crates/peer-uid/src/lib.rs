//! The uid of whoever is on the other end of a Unix socket.
//!
//! A socket in a shared temp directory is reachable by every local user, so a
//! daemon that serves whoever connects is serving the whole machine. This is the
//! one fact that tells them apart, and `std` does not expose it:
//! `UnixStream::peer_cred` is still unstable.
//!
//! Unix only. Windows has `GetNamedPipeClientProcessId`, but this crate does not
//! call it, so there the answer is "unknown" — and the caller has to decide what
//! that means, which is why this returns an `Option` rather than assuming.


/// What the kernel writes for `SO_PEERCRED` on Linux. The field order is the
/// ABI, not a choice; `pid_t` is `i32` on every platform this builds for.
/// Linux's `struct ucred`, and the options that reach it.
///
/// `cfg(target_os = "linux")`, not `cfg(unix)`: macOS is Unix too and answers
/// `SO_PEERCRED` with a *different* struct through a *different* level. Reading
/// one as the other is not a wrong answer, it is a failed `getsockopt` — and a
/// failed one returns `None`, which the daemon's gate then reads as "this
/// platform cannot say" and refuses every surface. That is what the `rust-macos`
/// job caught: fourteen daemon integration tests, all failing on
/// "adapter closed before hello".
#[cfg(target_os = "linux")]
#[repr(C)]
struct Ucred {
    pid: i32,
    uid: u32,
    gid: u32,
}

#[cfg(target_os = "linux")]
extern "C" {
    fn getsockopt(
        socket: std::os::raw::c_int,
        level: std::os::raw::c_int,
        option_name: std::os::raw::c_int,
        option_value: *mut Ucred,
        option_len: *mut std::os::raw::c_uint,
    ) -> std::os::raw::c_int;
}

/// `SOL_SOCKET` and `SO_PEERCRED` as Linux defines them.
#[cfg(target_os = "linux")]
const SOL_SOCKET: std::os::raw::c_int = 1;
#[cfg(target_os = "linux")]
const SO_PEERCRED: std::os::raw::c_int = 17;

/// macOS's `struct xucred`, reached through `SOL_LOCAL` / `LOCAL_PEERCRED`.
///
/// The layout is from `<sys/ucred.h>`: a version, the uid, a group count, then
/// the groups. Only the uid is read, for the reason the module doc gives — the
/// pid is not carried here at all, so there is nothing to be tempted by.
#[cfg(target_os = "macos")]
#[repr(C)]
struct Xucred {
    version: u32,
    uid: u32,
    ngroups: i16,
    groups: [u32; 16],
}

#[cfg(target_os = "macos")]
extern "C" {
    fn getsockopt(
        socket: std::os::raw::c_int,
        level: std::os::raw::c_int,
        option_name: std::os::raw::c_int,
        option_value: *mut Xucred,
        option_len: *mut std::os::raw::c_uint,
    ) -> std::os::raw::c_int;
}

/// `SOL_LOCAL` is 0 and `LOCAL_PEERCRED` is 1 on macOS. Both differ from the
/// Linux pair above, which is why they cannot share the constants.
#[cfg(target_os = "macos")]
const SOL_LOCAL: std::os::raw::c_int = 0;
#[cfg(target_os = "macos")]
const LOCAL_PEERCRED: std::os::raw::c_int = 0x001;

/// The peer's uid, or `None` where the platform cannot say.
///
/// uid only, never the pid: unix(7) says the pid is recorded at socket creation,
/// gets recycled, and stops describing the peer once that process changes
/// credentials — so it cannot carry an authorization decision on its own.
///
/// Generic over the socket type rather than taking a `UnixStream`, and that is
/// what makes this crate compile on Windows at all: `UnixStream` does not exist
/// there, so a signature naming it fails before any `#[cfg]` inside the body can
/// help. The daemon's Windows gate (`cargo clippy --target x86_64-pc-windows-msvc`)
/// was red for exactly this reason.
///
/// On Windows the answer is always `None`. A named pipe does carry a peer
/// credential — `GetNamedPipeClientProcessId` — but it answers a **pid**, and
/// this crate is uid-only for the reason unix(7) gives. Going from that pid to a
/// uid-equivalent is `OpenProcess` → `OpenProcessToken` →
/// `GetTokenInformation(TokenUser)` → SID, three more FFI calls. Unwritten on
/// purpose: the caller reads an unknown peer as a platform that cannot say, not
/// as a permitted one.
#[cfg(target_os = "linux")]
pub fn peer_uid<T: std::os::unix::io::AsRawFd>(stream: &T) -> Option<u32> {
    let mut cred = Ucred { pid: 0, uid: 0, gid: 0 };
    let mut len = std::mem::size_of::<Ucred>() as std::os::raw::c_uint;
    // SAFETY: `Ucred` is the struct the kernel fills for `SO_PEERCRED` on Linux;
    // `len` is its size; the fd comes from a live socket and is only borrowed for
    // the duration of the call. Nothing else touches either buffer, and the call
    // cannot block.
    let rc = unsafe {
        getsockopt(
            stream.as_raw_fd(),
            SOL_SOCKET,
            SO_PEERCRED,
            &mut cred,
            &mut len,
        )
    };
    if rc != 0 {
        return None;
    }
    Some(cred.uid)
}

/// macOS, through `SOL_LOCAL` / `LOCAL_PEERCRED` and `struct xucred`.
#[cfg(target_os = "macos")]
pub fn peer_uid<T: std::os::unix::io::AsRawFd>(stream: &T) -> Option<u32> {
    let mut cred = Xucred { version: 0, uid: 0, ngroups: 0, groups: [0; 16] };
    let mut len = std::mem::size_of::<Xucred>() as std::os::raw::c_uint;
    // SAFETY: `Xucred` is the struct the kernel fills for `LOCAL_PEERCRED` on
    // macOS; `len` is its size; the fd comes from a live socket and is only
    // borrowed for the duration of the call. Nothing else touches either buffer,
    // and the call cannot block.
    let rc = unsafe {
        getsockopt(
            stream.as_raw_fd(),
            SOL_LOCAL,
            LOCAL_PEERCRED,
            &mut cred,
            &mut len,
        )
    };
    if rc != 0 {
        return None;
    }
    Some(cred.uid)
}

/// Windows, and anything else without a peer-credential socket option.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn peer_uid<T>(_stream: &T) -> Option<u32> {
    None
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests {
    use super::*;

    #[test]
    fn a_socket_pair_reports_the_uid_on_the_other_end() {
        let (ours, theirs) = std::os::unix::net::UnixStream::pair().expect("a socket pair");
        // Both ends are this process, so both must report this process's uid.
        let uid = peer_uid(&ours).expect("the peer's uid");
        assert_eq!(uid, peer_uid(&theirs).expect("the peer's uid"));
        // Not zero, and not a default: the kernel actually filled it in.
        assert!(uid > 0, "the kernel reported uid {uid}");
    }
}

/// Why the Windows peer gate fails open, asserted where it can be.
///
/// `serve_surface` refuses a surface only when `peer_uid` answers `None` *and*
/// the platform is Unix. On Windows the answer is always `None`, so the gate
/// never fires. That is TH13, and until now nothing on Windows said so: this
/// crate's only test was Unix-gated, so the platform carrying the gap had no
/// test at all.
///
/// This is the deployment half of the PoC — it shows `peer_uid` really does
/// answer `None` for a live connection here, which is what routes
/// `serve_surface` into the accepting arm. The other half, that the arm then
/// accepts, is `an_unidentifiable_peer_is_accepted_on_windows_which_is_th13` in
/// the daemon.
#[cfg(all(test, not(any(target_os = "linux", target_os = "macos"))))]
mod windows_tests {
    use super::*;

    #[test]
    fn a_live_connection_reports_no_uid_on_windows() {
        // Whatever the transport hands back, the answer must be `None` — a
        // `Some` here would mean the platform *can* identify the peer, and the
        // `else if cfg!(unix)` arm in serve_surface would be wrong for a
        // different reason than the one recorded.
        let connected = std::fs::File::open(std::env::current_exe().expect("this binary"))
            .expect("some handle");
        assert!(
            peer_uid(&connected).is_none(),
            "Windows reported a peer uid, which would change what serve_surface does"
        );
    }
}
