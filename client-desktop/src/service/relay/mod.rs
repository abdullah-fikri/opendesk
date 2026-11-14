//! The relay connection (see `docs/features/relay-connection.md`).

mod connection;
mod dispatcher;
mod file_pacer;
mod resume;
mod signal_pacer;

pub use connection::spawn;
