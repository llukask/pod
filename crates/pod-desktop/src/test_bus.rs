//! A private D-Bus daemon for tests, so they need neither the system nor
//! the session bus. `dbus-daemon` comes from the dev shell, the Nix build's
//! check inputs and CI's packages.
//!
//! Clients authenticate as ANONYMOUS. With EXTERNAL, dbus-daemon looks up
//! the connecting user's seats in logind's /run/systemd files on a systemd
//! host, and the nono dev sandbox denies those, so the daemon would refuse
//! every client there. libdbus falls back to ANONYMOUS on its own; zbus
//! clients ask for it in [`connect`].

use std::any::Any;
use std::io::{BufRead as _, BufReader, Read as _};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// Runs `body` with the address of a fresh private bus, and fails the test
/// with the daemon's stderr if `body` panics or outlives `deadline`.
///
/// The body runs on a thread of its own with its own runtime, and this
/// thread holds the deadline. A blocking call on the body's thread would
/// stall a `tokio::time::timeout` sharing that thread, but not
/// `recv_timeout` here, and killing the daemon on timeout unblocks anything
/// still waiting on the bus.
pub(crate) fn run_on_private_bus<Body, Run>(deadline: Duration, body: Body)
where
    Body: FnOnce(String) -> Run + Send + 'static,
    Run: Future<Output = ()>,
{
    let (mut bus, stdout) = PrivateBus::start();
    let (finished, outcome) = mpsc::channel();
    let body = thread::spawn(move || {
        let address = read_address(stdout);
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime builds in tests")
            .block_on(body(address));
        let _ = finished.send(());
    });
    let failure = match outcome.recv_timeout(deadline) {
        Ok(()) => return,
        Err(RecvTimeoutError::Timeout) => format!("timed out after {deadline:?}"),
        Err(RecvTimeoutError::Disconnected) => body
            .join()
            .err()
            .map(panic_message)
            .unwrap_or_else(|| "the body stopped without finishing".to_owned()),
    };
    panic!("{failure}\ndbus-daemon stderr:\n{}", bus.stop());
}

/// A zbus connection to the private bus at `address`.
pub(crate) fn connect(address: &str) -> zbus::connection::Builder<'_> {
    zbus::connection::Builder::address(address)
        .expect("dbus-daemon prints a valid address")
        .auth_mechanism(zbus::AuthMechanism::Anonymous)
}

/// Dropping it kills the daemon.
#[derive(Debug)]
struct PrivateBus {
    daemon: Child,
    /// Drains the daemon's stderr, so a chatty daemon can't block on a
    /// full pipe, and keeps it for the failure message.
    stderr: Option<JoinHandle<String>>,
    _dir: tempfile::TempDir,
}

impl PrivateBus {
    /// Spawns the daemon without waiting for it. Its address arrives on
    /// the returned stdout once it accepts connections.
    fn start() -> (Self, ChildStdout) {
        let dir = tempfile::tempdir().expect("the temp dir is writable");
        // The policy is the stock session bus's. Its bare
        // `<allow eavesdrop>` is what lets clients receive replies.
        let config = dir.path().join("bus.conf");
        std::fs::write(
            &config,
            format!(
                "<busconfig>\
                   <type>session</type>\
                   <listen>unix:path={}</listen>\
                   <auth>ANONYMOUS</auth>\
                   <allow_anonymous/>\
                   <policy context=\"default\">\
                     <allow send_destination=\"*\" eavesdrop=\"true\"/>\
                     <allow eavesdrop=\"true\"/>\
                     <allow own=\"*\"/>\
                   </policy>\
                 </busconfig>",
                dir.path().join("bus").display()
            ),
        )
        .expect("the temp dir is writable");
        let mut daemon = Command::new("dbus-daemon")
            .arg("--nofork")
            .arg("--print-address")
            .arg(format!("--config-file={}", config.display()))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("dbus-daemon is on PATH (the dev shell and CI install it)");
        let mut stderr = daemon.stderr.take().expect("stderr is piped");
        let stderr = thread::spawn(move || {
            let mut output = String::new();
            let _ = stderr.read_to_string(&mut output);
            output
        });
        let stdout = daemon.stdout.take().expect("stdout is piped");
        let bus = Self {
            daemon,
            stderr: Some(stderr),
            _dir: dir,
        };
        (bus, stdout)
    }

    /// Kills the daemon and returns what it wrote to stderr.
    fn stop(&mut self) -> String {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
        self.stderr
            .take()
            .and_then(|drain| drain.join().ok())
            .unwrap_or_default()
    }
}

impl Drop for PrivateBus {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}

fn read_address(stdout: ChildStdout) -> String {
    let mut address = String::new();
    BufReader::new(stdout)
        .read_line(&mut address)
        .expect("dbus-daemon's stdout is readable");
    assert!(!address.is_empty(), "dbus-daemon exited before listening");
    address.trim().to_owned()
}

fn panic_message(payload: Box<dyn Any + Send>) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|message| (*message).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "the test panicked".to_owned())
}
