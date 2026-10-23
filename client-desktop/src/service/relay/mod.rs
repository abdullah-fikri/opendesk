//! The relay connection (see `docs/features/relay-connection.md`).

mod connection;
mod dispatcher;

pub use connection::spawn;
