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
        Ok(std::sync::Arc::new(
            miow::pipe::NamedPipeBuilder::new(endpoint)
                .first(false)
                .max_instances(1)
                .create()?,
        ))
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
