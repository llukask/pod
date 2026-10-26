//! A loopback port for each test server.
//!
//! The sandboxed dev environment can't bind port 0: it only lets processes
//! bind 127.0.0.1 on ports 3000-3010, 5440-5449, 8000-8099 and 10123 (see
//! CLAUDE.md). 3000 is the dev server, 8080 is taken and 5440-5449 belong to
//! Postgres, so test servers take the first free port of the rest. The bind
//! itself is the check, so two test processes (say, in two worktrees) never
//! end up on the same port.
//
// pod-desktop carries the same helper in `src/test_port.rs`; pod-protocol,
// their only shared crate, does no I/O.

use std::net::TcpListener;

/// Binds 127.0.0.1 on the port in `override_var` if it is set, otherwise on
/// the first free candidate port. The listener is nonblocking, ready for
/// `tokio::net::TcpListener::from_std`.
pub fn bind_test_port(override_var: &str) -> TcpListener {
    let listener = match std::env::var(override_var)
        .ok()
        .filter(|port| !port.is_empty())
    {
        Some(port) => {
            let port: u16 = port
                .parse()
                .unwrap_or_else(|_| panic!("{override_var} is a port number"));
            TcpListener::bind(("127.0.0.1", port))
                .unwrap_or_else(|error| panic!("{override_var}={port} is free: {error}"))
        }
        None => (3001..=3010)
            .chain((8000..=8099).filter(|port| *port != 8080))
            .find_map(|port| TcpListener::bind(("127.0.0.1", port)).ok())
            .expect("a test port in 3001-3010 or 8000-8099 is free"),
    };
    listener
        .set_nonblocking(true)
        .expect("a bound socket can switch to nonblocking");
    listener
}
