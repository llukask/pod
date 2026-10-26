//! Whether the OS has a network connection, and whether it is metered.
//!
//! The sync engine skips its timer tick while offline and shows "Offline"
//! (spec 5.6), and the reconciler parks downloads while offline and holds
//! back metered-unfriendly ones (spec 7.2, 7.4). Both read a
//! [`NetworkState`] from the [`NetworkMonitor`] global's watch channel.
//!
//! The OS view is a hint, not proof. A request that fails with a DNS or
//! connect error counts as "no connection" at the call site even while this
//! reports connected (spec 7.2), so erring towards "connected" costs one
//! failed request, while erring towards "offline" would stall sync.

use gpui_kit::Global;
use tokio::sync::watch;

use crate::runtime::Background;

/// What the OS reports about the network right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NetworkState {
    /// Some route to the network exists. Doesn't guarantee the server is
    /// reachable.
    pub connected: bool,
    /// The user or the OS marked the connection as costly (a phone hotspot,
    /// a cellular modem).
    pub metered: bool,
}

impl NetworkState {
    /// What the app assumes without an OS signal: no NetworkManager on the
    /// bus, no system bus, or a platform without a monitor yet.
    pub const ASSUMED: Self = Self {
        connected: true,
        metered: false,
    };

    /// Maps NetworkManager's `State` (`NM_STATE_*`) and `Metered`
    /// (`NM_METERED_*`) properties.
    #[cfg_attr(
        not(target_os = "linux"),
        expect(dead_code, reason = "only NetworkManager reports these")
    )]
    pub fn from_network_manager(state: u32, metered: u32) -> Self {
        // Values from NetworkManager's `nm-dbus-interface.h`.
        const NM_STATE_ASLEEP: u32 = 10;
        const NM_STATE_DISCONNECTED: u32 = 20;
        const NM_STATE_DISCONNECTING: u32 = 30;
        const NM_STATE_CONNECTING: u32 = 40;
        const NM_METERED_YES: u32 = 1;
        const NM_METERED_GUESS_YES: u32 = 3;

        // Only the states that rule out any route count as offline.
        // CONNECTED_LOCAL (no default route) and CONNECTED_SITE (NM's internet
        // check failed) still reach a server on the LAN or over a VPN, which
        // is where a self-hosted Pod often lives. UNKNOWN means NM itself
        // can't tell, and an unknown future value gets the same benefit of
        // the doubt.
        let connected = !matches!(
            state,
            NM_STATE_ASLEEP | NM_STATE_DISCONNECTED | NM_STATE_DISCONNECTING | NM_STATE_CONNECTING
        );
        // NM guesses "yes" for links it knows to be costly (a WWAN modem, a
        // phone's hotspot), and Android treats a guess the same way. UNKNOWN
        // is what NM reports with no active connection, so it isn't evidence
        // of cost.
        let metered = matches!(metered, NM_METERED_YES | NM_METERED_GUESS_YES);
        Self { connected, metered }
    }
}

/// The current [`NetworkState`], kept up to date on the background runtime.
/// Registered as a GPUI global.
#[derive(Debug)]
pub struct NetworkMonitor {
    /// Holding the sender keeps the channel open on platforms without a
    /// monitor, so receivers wait on `changed()` instead of seeing it closed.
    state: watch::Sender<NetworkState>,
}

impl Global for NetworkMonitor {}

impl NetworkMonitor {
    /// Starts following the OS network state. The value is
    /// [`NetworkState::ASSUMED`] until the OS answers.
    pub fn start(background: &Background) -> Self {
        let (state, _) = watch::channel(NetworkState::ASSUMED);
        #[cfg(target_os = "linux")]
        background.spawn(network_manager::follow_system_bus(state.clone()));
        // TODO: follow `NWPathMonitor` on macOS (`status == .satisfied` for
        // connected, `isExpensive || isConstrained` for metered). Until then
        // macOS reports `ASSUMED` and relies on request errors.
        #[cfg(not(target_os = "linux"))]
        let _ = background;
        Self { state }
    }

    /// A receiver for the sync engine or the reconciler to read the state
    /// from and await changes on.
    pub fn subscribe(&self) -> watch::Receiver<NetworkState> {
        self.state.subscribe()
    }
}

/// Follows NetworkManager's `State` and `Metered` over the D-Bus system bus.
#[cfg(target_os = "linux")]
mod network_manager {
    use color_eyre::eyre::{Result, WrapErr};
    use futures_util::{StreamExt as _, stream};
    use tokio::sync::watch;
    use zbus::proxy::CacheProperties;

    use super::NetworkState;

    #[zbus::proxy(
        interface = "org.freedesktop.NetworkManager",
        default_service = "org.freedesktop.NetworkManager",
        default_path = "/org/freedesktop/NetworkManager",
        gen_blocking = false
    )]
    trait NetworkManager {
        #[zbus(property)]
        fn state(&self) -> zbus::Result<u32>;

        #[zbus(property)]
        fn metered(&self) -> zbus::Result<u32>;
    }

    /// Runs until the system bus connection closes. Without a system bus the
    /// state stays [`NetworkState::ASSUMED`].
    pub async fn follow_system_bus(sender: watch::Sender<NetworkState>) {
        let connection = match zbus::Connection::system().await {
            Ok(connection) => connection,
            Err(error) => {
                tracing::warn!(
                    %error,
                    "no D-Bus system bus, assuming the network is connected and unmetered"
                );
                return;
            }
        };
        if let Err(error) = follow(&connection, &sender).await {
            tracing::warn!(
                error = format!("{error:#}"),
                "stopped following NetworkManager, assuming the network is connected and unmetered"
            );
        }
        publish(&sender, NetworkState::ASSUMED);
    }

    /// Publishes NetworkManager's state on `connection` to `sender`, and
    /// again on every change, until the connection closes. While NM isn't on
    /// the bus the state is [`NetworkState::ASSUMED`]; NM starting or
    /// restarting is picked up from its name changing owner.
    pub(super) async fn follow(
        connection: &zbus::Connection,
        sender: &watch::Sender<NetworkState>,
    ) -> Result<()> {
        // Properties are read fresh on every change rather than cached, so a
        // restarted NM can't leave a stale value behind.
        let manager = NetworkManagerProxy::builder(connection)
            .cache_properties(CacheProperties::No)
            .build()
            .await
            .context("create the NetworkManager proxy")?;
        let properties = zbus::fdo::PropertiesProxy::builder(connection)
            .destination(manager.inner().destination().to_owned())
            .context("address the NetworkManager service")?
            .path(manager.inner().path().to_owned())
            .context("address the NetworkManager object")?
            .build()
            .await
            .context("create the NetworkManager properties proxy")?;

        // Subscribing before the first read means a change that lands in
        // between still wakes the loop. Any `PropertiesChanged` triggers a
        // re-read, not only `State` and `Metered`: NM sends few of them, and
        // reading two properties costs less than tracking which changed.
        let owner_changes = manager
            .inner()
            .receive_owner_changed()
            .await
            .context("subscribe to NetworkManager starting and stopping")?
            .map(|_| ());
        let property_changes = properties
            .receive_properties_changed()
            .await
            .context("subscribe to NetworkManager property changes")?
            .map(|_| ());
        let mut changes = stream::select(owner_changes, property_changes);

        let mut reported_absent = false;
        loop {
            let next = match read(&manager).await {
                Ok(state) => {
                    reported_absent = false;
                    state
                }
                Err(error) => {
                    if !reported_absent {
                        tracing::warn!(
                            %error,
                            "NetworkManager isn't answering, assuming the network is connected and unmetered"
                        );
                        reported_absent = true;
                    }
                    NetworkState::ASSUMED
                }
            };
            publish(sender, next);
            if changes.next().await.is_none() {
                return Ok(());
            }
        }
    }

    async fn read(manager: &NetworkManagerProxy<'_>) -> zbus::Result<NetworkState> {
        Ok(NetworkState::from_network_manager(
            manager.state().await?,
            manager.metered().await?,
        ))
    }

    /// Sends `next` if it differs from the current value. The log line is how
    /// to watch the monitor by hand (`RUST_LOG=pod_desktop::net=info`).
    fn publish(sender: &watch::Sender<NetworkState>, next: NetworkState) {
        sender.send_if_modified(|current| {
            let changed = *current != next;
            if changed {
                tracing::info!(
                    connected = next.connected,
                    metered = next.metered,
                    "network state changed"
                );
                *current = next;
            }
            changed
        });
    }

    #[cfg(test)]
    mod tests {
        use std::any::Any;
        use std::io::{BufRead as _, BufReader, Read as _};
        use std::process::{Child, ChildStdout, Command, Stdio};
        use std::sync::mpsc::{self, RecvTimeoutError};
        use std::thread::{self, JoinHandle};
        use std::time::Duration;

        use zbus::object_server::SignalEmitter;

        use super::*;

        const PATH: &str = "/org/freedesktop/NetworkManager";
        const NM_STATE_DISCONNECTED: u32 = 20;
        const NM_STATE_CONNECTED_GLOBAL: u32 = 70;
        const NM_METERED_GUESS_YES: u32 = 3;
        const NM_METERED_GUESS_NO: u32 = 4;

        /// Bounds the whole test, blocking calls included.
        const TEST_DEADLINE: Duration = Duration::from_secs(30);
        /// Bounds each wait for the channel to reach a value.
        const STEP_DEADLINE: Duration = Duration::from_secs(5);

        /// A `dbus-daemon` of the test's own, so the test needs neither the
        /// system bus nor a real NetworkManager. Dropping it kills the daemon.
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
                           <auth>EXTERNAL</auth>\
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

        fn connect(address: &str) -> zbus::connection::Builder<'_> {
            zbus::connection::Builder::address(address).expect("dbus-daemon prints a valid address")
        }

        struct FakeNetworkManager {
            state: u32,
            metered: u32,
        }

        #[zbus::interface(name = "org.freedesktop.NetworkManager")]
        impl FakeNetworkManager {
            #[zbus(property)]
            fn state(&self) -> u32 {
                self.state
            }

            #[zbus(property)]
            fn metered(&self) -> u32 {
                self.metered
            }
        }

        async fn start_network_manager(address: &str, state: u32) -> zbus::Connection {
            connect(address)
                .serve_at(
                    PATH,
                    FakeNetworkManager {
                        state,
                        metered: NM_METERED_GUESS_NO,
                    },
                )
                .expect("the path is valid")
                .name("org.freedesktop.NetworkManager")
                .expect("the name is valid")
                .build()
                .await
                .expect("the fake NetworkManager connects to the private bus")
        }

        async fn wait_for(
            receiver: &mut watch::Receiver<NetworkState>,
            expected: NetworkState,
            step: &str,
        ) {
            let waited = tokio::time::timeout(STEP_DEADLINE, receiver.wait_for(|s| *s == expected))
                .await
                .map(|result| result.map(|_| ()));
            match waited {
                Ok(result) => result.expect("the monitor holds the sender"),
                Err(_) => panic!("{step}: still {:?}, want {expected:?}", *receiver.borrow()),
            }
        }

        fn panic_message(payload: Box<dyn Any + Send>) -> String {
            payload
                .downcast_ref::<&str>()
                .map(|message| (*message).to_owned())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "the test panicked".to_owned())
        }

        // dbus-daemon looks up the connecting user's seats in logind's
        // /run/systemd files on a systemd host, and the nono dev sandbox denies
        // those, so the daemon refuses every client there. `nix build
        // .#pod-desktop` and CI run it with `--include-ignored`.
        //
        // The body runs on a thread of its own with its own runtime, and this
        // thread holds the deadline. A blocking call on the body's thread
        // would stall a `tokio::time::timeout` sharing that thread, but not
        // `recv_timeout` here, and killing the daemon on timeout unblocks
        // anything still waiting on the bus.
        #[test]
        #[ignore = "dbus-daemon needs logind's /run/systemd, denied in the dev sandbox"]
        fn the_channel_follows_network_manager_coming_changing_and_going() {
            let (mut bus, stdout) = PrivateBus::start();
            let (finished, outcome) = mpsc::channel();
            let body = thread::spawn(move || {
                let address = read_address(stdout);
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("a runtime builds in tests")
                    .block_on(follow_network_manager(&address));
                let _ = finished.send(());
            });
            let failure = match outcome.recv_timeout(TEST_DEADLINE) {
                Ok(()) => return,
                Err(RecvTimeoutError::Timeout) => format!("timed out after {TEST_DEADLINE:?}"),
                Err(RecvTimeoutError::Disconnected) => body
                    .join()
                    .err()
                    .map(panic_message)
                    .unwrap_or_else(|| "the body stopped without finishing".to_owned()),
            };
            panic!("{failure}\ndbus-daemon stderr:\n{}", bus.stop());
        }

        async fn follow_network_manager(address: &str) {
            let monitor_connection = connect(address)
                .build()
                .await
                .expect("the monitor connects to the private bus");
            let (sender, mut receiver) = watch::channel(NetworkState::ASSUMED);
            let monitor = tokio::spawn(async move { follow(&monitor_connection, &sender).await });

            let offline = NetworkState {
                connected: false,
                metered: false,
            };
            let network_manager = start_network_manager(address, NM_STATE_DISCONNECTED).await;
            wait_for(&mut receiver, offline, "NetworkManager appears").await;

            let manager = network_manager
                .object_server()
                .interface::<_, FakeNetworkManager>(PATH)
                .await
                .expect("the fake is served at PATH");
            let emitter: &SignalEmitter<'_> = manager.signal_emitter();
            {
                let mut fake = manager.get_mut().await;
                fake.state = NM_STATE_CONNECTED_GLOBAL;
                fake.metered = NM_METERED_GUESS_YES;
                fake.state_changed(emitter).await.expect("the signal sends");
                fake.metered_changed(emitter)
                    .await
                    .expect("the signal sends");
            }
            let metered = NetworkState {
                connected: true,
                metered: true,
            };
            wait_for(&mut receiver, metered, "Wi-Fi up on a metered link").await;

            drop(manager);
            network_manager.close().await.expect("the fake disconnects");
            wait_for(&mut receiver, NetworkState::ASSUMED, "NetworkManager stops").await;

            let _restarted = start_network_manager(address, NM_STATE_DISCONNECTED).await;
            wait_for(&mut receiver, offline, "NetworkManager restarts").await;

            monitor.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_manager_states_map_to_connected() {
        let cases = [
            (0, true, "unknown: NM can't tell, so assume connected"),
            (10, false, "asleep"),
            (20, false, "disconnected"),
            (30, false, "disconnecting"),
            (40, false, "connecting"),
            (50, true, "connected_local: a LAN-only server is reachable"),
            (60, true, "connected_site: connectivity check failed"),
            (70, true, "connected_global"),
            (99, true, "a future state: assume connected"),
        ];
        for (state, connected, case) in cases {
            assert_eq!(
                NetworkState::from_network_manager(state, 4).connected,
                connected,
                "{case}"
            );
        }
    }

    #[test]
    fn network_manager_metered_values_map_to_metered() {
        let cases = [
            (0, false, "unknown"),
            (1, true, "yes"),
            (2, false, "no"),
            (3, true, "guess_yes"),
            (4, false, "guess_no"),
            (99, false, "a future value"),
        ];
        for (metered, expected, case) in cases {
            assert_eq!(
                NetworkState::from_network_manager(70, metered).metered,
                expected,
                "{case}"
            );
        }
    }
}
