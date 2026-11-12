//! The resident host daemon.
//!
//! The Tauri shell is a client. The daemon owns the Studio Bridge endpoint and
//! the runtime adapter behind it, so killing the shell does not kill the
//! conversation: the shell reconnects and continues.
//!
//! M2.1 is the lifecycle only — acquire the endpoint, supervise the adapter,
//! relay frames to whichever shell is attached. Terminating the protocol
//! (session multiplexing, then models/usage/settings/diagnostics) is M2.3 and
//! M2.4, which replace the relay rather than sit under it.

pub mod adapter;
pub mod lifecycle;
pub mod process;
pub mod relay;
pub mod state;
pub mod supervisor;
