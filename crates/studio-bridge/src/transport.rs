//! The local socket the bridge runs on.
//!
//! Unix domain sockets on Unix; named pipes on Windows, which `std` does not
//! provide. The two are not interchangeable at the API level — a named pipe has
//! no `set_read_timeout`, for one — so every difference is confined here and the
//! rest of the crate sees one pair of types.
//!
//! The endpoint string stays a plain path on both: on Windows it is translated
//! to the `\\.\pipe\<name>` form the pipe API expects, so the caller never has
//! to know either shape.
//!
//! A Windows pipe is **duplex**, and `miow::pipe::NamedPipe` implements both
//! `Read` and `Write` on `&self`. So one connection is shared behind an `Arc`
//! and the reader and writer threads each hold a clone of that — the same shape
//! Unix gets from `try_clone`.
//!
//! This module used to claim the opposite ("a pipe handle serves one direction")
//! and built `reopen` plus a `STEP_STUDIO_TRANSPORT_ENDPOINT` environment
//! variable to work around it. Both are deleted. The premise was wrong — but
//! under `named_pipe 0.4.1`, the crate this used to depend on, the workaround
//! was *forced*: its `PipeServer` was not `Clone`, exposed no raw handle, and
//! took `&mut self` for both directions. What was wrong was blaming Windows for
//! a crate's API, not the instinct that something had to be worked around.

use std::io;
use std::time::Duration;

#[cfg(unix)]
mod imp {
    /// One end of a Unix socket: the same type whether it connected or accepted.
    pub type Stream = std::os::unix::net::UnixStream;
    pub type Listener = std::os::unix::net::UnixListener;
}

#[cfg(windows)]
mod imp {
    /// The connecting side. `miow::pipe::connect` hands back a `File` opened
    /// read+write, and a `File` is already `Read + Write` and clones with
    /// `try_clone` — so the client needs nothing pipe-specific and never has to
    /// reach for a raw handle. (`miow::pipe::NamedPipe` could only be built from
    /// one, and the workspace forbids `unsafe`.)
    pub type Stream = std::fs::File;
    /// What `bind` hands back: the server's pipe, behind an `Arc` because
    /// `Read` and `Write` are both on `&self`, so the reader thread and the
    /// writer thread each hold a clone of this instead of a handle each.
    pub type Listener = std::sync::Arc<miow::pipe::NamedPipe>;
}

/// Both ends of a pipe read and write the same bytes, and so does a Unix socket,
/// so the frame codec — which is generic over `Read + Write` — can take whatever
/// was accepted.
impl std::io::Read for Accepted {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            #[cfg(unix)]
            Self::Stream(stream) => stream.read(buf),
            #[cfg(windows)]
            Self::Stream(stream) => stream.read(buf),
            #[cfg(windows)]
            Self::Server(server) => std::io::Read::read(&mut &**server, buf),
        }
    }
}

impl std::io::Write for Accepted {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            #[cfg(unix)]
            Self::Stream(stream) => stream.write(buf),
            #[cfg(windows)]
            Self::Stream(stream) => stream.write(buf),
            #[cfg(windows)]
            Self::Server(server) => std::io::Write::write(&mut &**server, buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            #[cfg(unix)]
            Self::Stream(stream) => stream.flush(),
            #[cfg(windows)]
            Self::Stream(stream) => stream.flush(),
            #[cfg(windows)]
            Self::Server(server) => std::io::Write::flush(&mut &**server),
        }
    }
}

/// A second handle on the same connection, so a reader thread can own one while
/// the session writes through the other.
///
/// Both platforms duplicate: a Unix socket with `try_clone`, a Windows pipe
/// because `miow::pipe::connect` returns a `File`, and `File::try_clone` is
/// `DuplicateHandle` under the hood. The server side shares an `Arc` instead —
/// see [`Listener`].
pub fn duplicate(stream: &Stream) -> io::Result<Stream> {
    stream.try_clone()
}

impl Accepted {
    /// Whatever the platform accepted, as the stream the session talks over.
    #[cfg(unix)]
    pub fn into_connection(self) -> Connection {
        match self {
            Self::Stream(stream) => stream,
        }
    }

    #[cfg(windows)]
    pub fn into_connection(self) -> Connection {
        // Only the server variant is produced by `accept`; the client variant
        // exists so one enum covers both platforms' call sites.
        match self {
            Self::Stream(_) => unreachable!("the Windows accept yields a server"),
            Self::Server(server) => PipeHandle(server),
        }
    }
}

/// A connection the runtime accepted.
///
/// On Unix both ends are the same type. On Windows they are too, so the enum
/// keeps both variants only to give the two platforms one shape.
#[cfg(windows)]
pub enum Accepted {
    Stream(Stream),
    Server(Listener),
}

#[cfg(unix)]
pub enum Accepted {
    Stream(Stream),
}

pub use imp::{Listener, Stream};

/// What [`Accepted::into_connection`] hands back: the connection a session
/// talks over.
///
/// Named once so a caller that holds one does not have to repeat the platform
/// split to say what type it has — and on Windows it is a wrapper rather than
/// the `Arc` itself, so that `Read` and `Write` are on `&mut self` exactly as
/// they are for a `UnixStream`. Without it every caller would need `&*x` on one
/// platform and `&x` on the other.
#[cfg(unix)]
pub type Connection = Stream;

/// The accepted pipe, behind the `Arc` that lets a reader thread and a writer
/// thread share one connection.
///
/// `miow::pipe::NamedPipe` implements `Read` and `Write` for `&NamedPipe`
/// rather than for itself, so the `Arc` does not implement either. This wrapper
/// forwards both onto `&mut self`, which is what keeps the daemon's read/write
/// split identical on both platforms — `UnixStream` takes `&self` for both, but
/// a caller holding either type writes `x.write_all(..)` and reads
/// `next_frame(&mut x)` the same way.
#[cfg(windows)]
pub struct PipeHandle(std::sync::Arc<miow::pipe::NamedPipe>);

#[cfg(windows)]
impl std::io::Read for PipeHandle {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        std::io::Read::read(&mut &*self.0, buf)
    }
}

#[cfg(windows)]
impl std::io::Write for PipeHandle {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        std::io::Write::write(&mut &*self.0, buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        std::io::Write::flush(&mut &*self.0)
    }
}

#[cfg(windows)]
impl PipeHandle {
    /// Half-close the pipe, the Windows counterpart of `shutdown(Both)`.
    ///
    /// `DisconnectNamedPipe`: the client's next read returns EOF, which is what
    /// lets the other side notice this one went away. Without it a Windows
    /// surface's reader blocks in `read` forever after the daemon stops writing.
    pub fn disconnect(&self) -> io::Result<()> {
        self.0.disconnect()
    }
}

#[cfg(windows)]
pub type Connection = PipeHandle;

/// What a client connects as, on either platform.
pub type Transport = Stream;

// A behaviour difference the `named_pipe` → `miow` swap introduced, worth naming
// because it is silent.
//
// `named_pipe 0.4.1` normalised `ERROR_BROKEN_PIPE` into `Ok(0)` — a clean EOF —
// in both its `Read` and `Write` impls (`lib.rs:450`, `:1029`). `miow` does not:
// a read on a pipe whose peer has disconnected returns `Err(ERROR_BROKEN_PIPE)`.
//
// So a reader that used to see `Ok(0)` now sees an error. All three readers in
// the tree handle the error path as "closed" — the shell's `spawn_reader`
// (`client.rs`), the daemon's relay loop (`bin/studio-host.rs`), the runtime's
// pump (`bridge_server.rs`) — but that handling used to be a fallback and is now
// the path a normal disconnect takes. Anything added later that matches only on
// `Ok(None)` will silently miss a closed pipe.

/// Close one end of a connection so the other side observes it.
///
/// `shutdown(Both)` on Unix; `DisconnectNamedPipe` on Windows. This is the
/// operation the daemon's `close_socket` performs, lifted here so the platform
/// difference lives with all the others instead of in a caller.
///
/// What the peer sees differs, and that is worth knowing before relying on it:
/// Microsoft documents that after `DisconnectNamedPipe` the client "receives an
/// error the next time it attempts to access the pipe" — not a clean EOF. Every
/// reader in the tree treats a read error as closed, but that is now the path a
/// normal disconnect takes rather than a fallback.
#[cfg(unix)]
pub fn close_connection(connection: &Connection) -> io::Result<()> {
    connection.shutdown(std::net::Shutdown::Both)
}

#[cfg(windows)]
pub fn close_connection(connection: &Connection) -> io::Result<()> {
    connection.disconnect()
}

/// A second usable handle on an accepted connection, so one thread can read
/// while another writes.
///
/// On Unix this duplicates the socket. On Windows the connection is a pipe
/// behind an `Arc` and `Read` and `Write` are both on `&self`, so a clone of the
/// `Arc` is the second handle — and both threads reach the I/O through a
/// deref, which is why [`Connection`] is handed out rather than the pipe type.
#[cfg(unix)]
pub fn connection_duplicate(connection: &Connection) -> io::Result<Connection> {
    connection.try_clone()
}

#[cfg(windows)]
pub fn connection_duplicate(connection: &Connection) -> io::Result<Connection> {
    Ok(PipeHandle(std::sync::Arc::clone(&connection.0)))
}

/// Windows named pipes live in the `\\.\pipe\` namespace and are named, not
/// pathed; a Unix socket takes the path exactly as given.
#[cfg(windows)]
pub fn endpoint_for_bind(endpoint: &str) -> String {
    if endpoint.starts_with("\\\\") || endpoint.starts_with("//") {
        return endpoint.to_string();
    }
    let name = endpoint
        .rsplit(['/', '\\'])
        .find(|part| !part.is_empty())
        .unwrap_or("step-studio");
    format!("\\\\.\\pipe\\{name}")
}

#[cfg(unix)]
pub fn endpoint_for_bind(endpoint: &str) -> String {
    endpoint.to_string()
}

/// Whether something is listening at `endpoint`, without disturbing it.
///
/// On Unix a connect is harmless and answers the question directly. On Windows
/// a connect *consumes* a pipe instance: the server has to serve that client
/// and rebind before the next one gets through, so a readiness probe that
/// connects wedges the rebind with ERROR_PIPE_BUSY — which is what happened to
/// the runtime's e2e suite the moment its probe stopped being `Path::exists`.
/// `WaitNamedPipe` asks without taking one.
pub fn is_listening(endpoint: &str) -> bool {
    #[cfg(unix)]
    {
        std::os::unix::net::UnixStream::connect(endpoint).is_ok()
    }
    #[cfg(windows)]
    {
        // `endpoint_for_bind` first: `WaitNamedPipeW` takes the same
        // `\\.\pipe\` name `CreateNamedPipeW` does, and is idempotent on a
        // name that already carries it.
        let name = endpoint_for_bind(endpoint);
        miow::pipe::wait(&name, Some(std::time::Duration::ZERO)).is_ok()
    }
}

/// Connect to a listener at `endpoint`.
pub fn connect(endpoint: &str) -> io::Result<Stream> {
    #[cfg(unix)]
    {
        std::os::unix::net::UnixStream::connect(endpoint)
    }
    #[cfg(windows)]
    {
        // Blocks until the server is reachable, and hands back the read+write
        // `File` the client talks over.
        miow::pipe::connect(endpoint)
    }
}

/// A read timeout. Named pipes block until data arrives with no equivalent, so
/// the setting is accepted and ignored rather than silently not applying.
pub fn set_read_timeout(stream: &Stream, timeout: Option<Duration>) -> io::Result<()> {
    #[cfg(unix)]
    {
        stream.set_read_timeout(timeout)
    }
    #[cfg(windows)]
    {
        let _ = (stream, timeout);
        Ok(())
    }
}

/// Something the handshake can write one frame through, before the session owns
/// the socket.
///
/// On Unix the stream is duplicable, so this is a second handle to the same
/// socket. On Windows the client side is a `std::fs::File`, which is `Clone`
/// through `try_clone`, but the handshake frame is the only thing written before
/// the reader thread starts — so the caller lends the one it already holds
/// rather than spending a duplicate.
#[cfg(unix)]
pub fn handshake_writer(stream: &Stream) -> Stream {
    stream
        .try_clone()
        .expect("the handshake handle duplicates")
}

#[cfg(windows)]
pub fn handshake_writer(stream: &mut Stream) -> &mut Stream {
    stream
}

/// Bind a listener at `endpoint`.
///
/// On Unix the listener persists and accepts repeatedly, so the value returned
/// is a listener. On Windows the pipe is created here and [`accept`] is what
/// waits for a client, because `NamedPipe::connect` takes `&self` — so the
/// listener survives the wait and the serve loop re-accepts on the same one.
///
/// Note that on Windows a *second* `bind` for the same name needs
/// `FILE_FLAG_FIRST_PIPE_INSTANCE` cleared, which is why the Windows arm builds
/// the pipe explicitly. See the comment there.
pub fn bind(endpoint: &str) -> io::Result<Listener> {
    #[cfg(unix)]
    {
        Listener::bind(endpoint)
    }
    #[cfg(windows)]
    {
        // `first(false)`, not `NamedPipe::new`: the builder defaults to
        // `FILE_FLAG_FIRST_PIPE_INSTANCE`, which makes a *second* instance of
        // the same name fail with ERROR_ACCESS_DENIED — so the rebind both
        // serve loops do before handing a connection off would die after the
        // first client, and the daemon would stop being resident.
        //
        // What that gives up: "fail if this name is already taken". On Unix
        // `lifecycle::acquire` provides that by probing the endpoint before
        // binding; on Windows the probe still runs, but nothing stops a second
        // process from creating another instance of the name, and the default
        // DACL lets anyone try. That is S1, and it is open.
        // `max_instances(1)`, because the builder defaults to
        // `PIPE_UNLIMITED_INSTANCES` (255) and that default is inert only while
        // `first(true)` caps the pipe at one. Clearing the flag — which the
        // rebind needs — makes 255 live, and `serve_surface` is deliberately
        // one-surface-at-a-time: a second surface waits rather than competing.
        // 255 instances of one name would let it compete.
        //
        // `endpoint_for_bind` first, so a caller may hand `bind` either the
        // socket path or the pipe name. `CreateNamedPipeW` takes a name in the
        // `\\.\pipe\` namespace and nothing else, so a path here is
        // ERROR_INVALID_NAME — and that mistake has now been made in seven test
        // sites, each of which passed the path and the pipe name to different
        // calls. The conversion is idempotent: a name that already carries the
        // prefix is returned unchanged, which is what `serve` hands in.
        let name = endpoint_for_bind(endpoint);
        Ok(std::sync::Arc::new(
            miow::pipe::NamedPipeBuilder::new(&name)
                .first(false)
                .max_instances(1)
                .create()?,
        ))
    }
}

/// How long each wait lasts before the stop flag is re-read.
///
/// Long enough that an arriving client does not wait on the poll, short enough
/// that a stop is observed without a perceptible delay.
const STOP_POLL: std::time::Duration = std::time::Duration::from_millis(50);

/// Run `blocked` on a thread and wait for it, giving up when `stop` says so.
///
/// `None` means stopped; `Some(result)` is whatever `blocked` produced.
///
/// This exists for the Windows accept. `NamedPipe::connect` is
/// `ConnectNamedPipe`, which blocks with no timeout, so a serve loop calling it
/// directly cannot observe its own stop flag — and this daemon's `serve` runs on
/// the main thread with `shutdown` after it, so a daemon parked there never
/// exited: not on the grace clock, not on an upgrade marker.
///
/// The thread is abandoned when `stop` fires. That is sound because the caller is
/// on its way out and the process takes the blocked syscall with it when `main`
/// returns — a thread stuck in a syscall does not hold a process open.
///
/// Generic over the blocking call rather than naming either platform's type, so
/// the part that can actually be wrong — the poll, the flag, the channel — is one
/// implementation with one test, and only the syscall differs per platform.
pub fn wait_or_stop<T, F>(blocked: F, stop: &std::sync::atomic::AtomicBool) -> Option<io::Result<T>>
where
    F: FnOnce() -> io::Result<T> + Send + 'static,
    T: Send + 'static,
{
    use std::sync::atomic::Ordering;
    use std::sync::mpsc;

    let (produced_tx, produced_rx) = mpsc::channel();
    std::thread::spawn(move || {
        // The receiver may already be gone, which is the stop case: a failed
        // send is the expected outcome there, not something to report.
        let _ = produced_tx.send(blocked());
    });

    loop {
        if stop.load(Ordering::Relaxed) {
            return None;
        }
        match produced_rx.recv_timeout(STOP_POLL) {
            Ok(produced) => return Some(produced),
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            // The waiter vanished without sending, which means it panicked.
            // Reporting that beats spinning on a channel nobody will write to.
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Some(Err(io::Error::other("the blocking call's thread ended")));
            }
        }
    }
}

/// Read one frame, giving up after `budget`.
///
/// `set_read_timeout` bounds a read on Unix and is accepted-and-ignored on
/// Windows, so a bound that holds on both platforms has to come from the wait
/// rather than from the socket option. Built on [`wait_or_stop`], which is
/// where the abandoning actually happens: the read runs on a detached thread,
/// so a peer that never sends does not hold its caller.
///
/// The reader is consumed because the closure must be `'static`. Sound when the
/// caller can hand over ownership — either it owns the reader outright, or it
/// duplicates the connection with [`Accepted::duplicate`].
pub fn first_frame_within<R>(
    mut reader: R,
    budget: Duration,
) -> Option<io::Result<Option<crate::frame::DecodedFrame>>>
where
    R: std::io::Read + Send + 'static,
{
    use std::sync::atomic::{AtomicBool, Ordering};
    let stop = std::sync::Arc::new(AtomicBool::new(false));
    let setter = std::sync::Arc::clone(&stop);
    std::thread::spawn(move || {
        std::thread::sleep(budget);
        setter.store(true, Ordering::Relaxed);
    });
    wait_or_stop(
        move || {
            crate::frame::read_frame(&mut reader).map_err(|error| io::Error::other(error.to_string()))
        },
        &stop,
    )
}

/// Wait for a client, giving up when `stop` says so.
///
/// The Windows counterpart of [`accept`], which blocks in `ConnectNamedPipe`
/// with no timeout and so cannot honour a stop flag. See [`wait_or_stop`].
#[cfg(windows)]
pub fn accept_until(
    listener: &Listener,
    stop: &std::sync::atomic::AtomicBool,
) -> io::Result<Option<Accepted>> {
    // `connect` takes `&self`, so the listener is shared rather than moved —
    // the same reason the reader and writer threads can hold one pipe.
    let shared = std::sync::Arc::clone(listener);
    match wait_or_stop(move || shared.connect(), stop) {
        None => Ok(None),
        Some(Ok(())) => Ok(Some(Accepted::Server(std::sync::Arc::clone(listener)))),
        Some(Err(error)) => Err(error),
    }
}

/// The connection the listener is holding.
///
/// On Unix this blocks until a client connects, as many times as it is called.
/// On Windows it blocks in `NamedPipe::connect`, which is `ConnectNamedPipe`.
///
/// Two things this does not do, both pre-existing and both unverified on
/// Windows: it has no timeout, so a Windows serve loop parked here never
/// observes `stopping` (the Unix arm sets the listener non-blocking for exactly
/// that reason); and no caller ever calls `disconnect`, so the
/// "re-accept on the same listener" path is reasoned about rather than
/// exercised.
pub fn accept(listener: Listener) -> io::Result<Accepted> {
    #[cfg(unix)]
    {
        listener
            .accept()
            .map(|(stream, _)| Accepted::Stream(stream))
    }
    #[cfg(windows)]
    {
        listener.connect()?;
        Ok(Accepted::Server(listener))
    }
}

/// A handle the handshake can read one frame from, before the session owns the
/// socket.
///
/// On Unix the stream is duplicable and `Read` is on `&self`, so this is a
/// second handle to the same socket. On Windows `Read` is on `&self` too, so the
/// accepted connection is lent as-is: this returns the *same* handle reborrowed,
/// not a second one, and the handshake is the only read before the reader thread
/// starts so there is nothing to collide with.
#[cfg(unix)]
pub fn handshake_reader(stream: &mut Accepted) -> impl std::io::Read + '_ {
    stream
}

#[cfg(windows)]
pub fn handshake_reader(stream: &mut Accepted) -> &mut Accepted {
    stream
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stop flag nobody has set yet.
    fn running() -> std::sync::atomic::AtomicBool {
        std::sync::atomic::AtomicBool::new(false)
    }

    #[test]
    fn wait_or_stop_returns_what_the_blocking_call_produced() {
        let stop = running();
        let produced = wait_or_stop(|| Ok(7u32), &stop).expect("not stopped");
        assert_eq!(produced.expect("the call succeeded"), 7);
    }

    #[test]
    fn wait_or_stop_passes_the_error_through_rather_than_swallowing_it() {
        let stop = running();
        let produced = wait_or_stop(
            || Err::<u32, _>(io::Error::other("the call failed")),
            &stop,
        )
        .expect("not stopped");
        assert!(produced.is_err(), "the error was dropped");
    }

    #[test]
    fn wait_or_stop_gives_up_once_the_stop_flag_is_set() {
        // The whole point of the helper: a call that would block forever must
        // still return once the flag is set.
        let stop = std::sync::Arc::new(running());
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();

        // The flag has to be set by another thread while `wait_or_stop` is
        // running. Setting it after the call returns is the deadlock this test
        // first shipped with: the call only returns *because* the flag was set.
        let setter = std::sync::Arc::clone(&stop);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            setter.store(true, std::sync::atomic::Ordering::Relaxed);
        });

        let started = std::time::Instant::now();
        let outcome = wait_or_stop(
            move || {
                // Never sends, so this blocks until the thread is abandoned.
                let _ = release_rx.recv();
                Ok(())
            },
            &stop,
        );
        // Let the abandoned waiter finish, so the test does not leak a thread.
        let _ = release_tx.send(());

        assert!(outcome.is_none(), "the stop was not honoured");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "the stop took {:?}",
            started.elapsed()
        );
    }
}
