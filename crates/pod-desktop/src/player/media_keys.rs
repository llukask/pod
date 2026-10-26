//! Media keys and the OS now-playing widget (spec 8), through `souvlaki`:
//! MPRIS on Linux, so `playerctl` and the GNOME and KDE media widgets show
//! the episode in the player and control it.
//!
//! Both directions are pure mappings. [`now_playing`] turns the player's
//! state into what the OS shows, and [`remote`] turns an OS request into a
//! [`Remote`] command, which [`apply`] runs on the player. [`OsControls`]
//! is the souvlaki boundary, and [`install`] wires it to the player.
//!
//! TODO: macOS and Windows come through the same crate but are untested.
//! Windows needs the main window's HWND in `PlatformConfig`, and macOS
//! needs the controls created on the main thread.

use std::path::{Path, PathBuf};
use std::time::Duration;

use color_eyre::eyre::{Result, WrapErr as _, eyre};
use gpui_kit::{App, AppContext as _, Context, Entity, Global, Subscription, Task};
use souvlaki::{
    MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig,
    SeekDirection,
};
use tokio::sync::mpsc;

use super::{Current, Player};
use crate::reconcile::Reconciler;

/// The MPRIS bus name is `org.mpris.MediaPlayer2.` followed by this.
pub const DBUS_NAME: &str = "pod";

/// How far a playing episode moves before the OS hears its position.
///
/// MPRIS clients read `Position` when they need it, and souvlaki answers
/// with the last position it was sent, so it needs updates while playing.
/// But its libdbus thread takes one update per pass and then waits up to a
/// second for D-Bus traffic, so sending every second (or faster, at 2x)
/// would queue updates behind each other and the widget would fall ever
/// further behind. Five seconds of audio is under one update a second up
/// to 3x speed.
const PLAYING_STEP_MS: i64 = 5_000;

// ==============================================================================
// Player state to the OS
// ==============================================================================

/// What the OS shows for the episode in the player.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NowPlaying {
    title: String,
    show: String,
    artwork_url: Option<String>,
    duration_ms: Option<i64>,
    /// In whole seconds while paused, as the player bar shows it, and in
    /// [`PLAYING_STEP_MS`] while playing.
    position_ms: i64,
    playing: bool,
}

/// What the OS shows for `current`. `None` with nothing in the player.
/// Cached artwork is a `file://` URL under `artwork_root`, so the widget
/// shows it offline.
pub fn now_playing(current: Option<&Current>, artwork_root: &Path) -> Option<NowPlaying> {
    let current = current?;
    let playing = current.is_playing();
    let step_ms = if playing { PLAYING_STEP_MS } else { 1_000 };
    Some(NowPlaying {
        title: current.episode.title.clone(),
        show: current.episode.podcast_title.clone(),
        artwork_url: match &current.episode.artwork_file {
            Some(file) => url::Url::from_file_path(artwork_root.join(file))
                .ok()
                .map(String::from),
            None => current.episode.artwork_url.clone(),
        },
        duration_ms: current.duration_ms(),
        position_ms: current.position_ms() / step_ms * step_ms,
        playing,
    })
}

impl NowPlaying {
    /// The show goes in both the artist and the album: widgets show one or
    /// the other under the title.
    pub fn metadata(&self) -> MediaMetadata<'_> {
        MediaMetadata {
            title: Some(&self.title),
            album: Some(&self.show),
            artist: Some(&self.show),
            cover_url: self.artwork_url.as_deref(),
            duration: self.duration_ms.and_then(duration),
        }
    }
}

/// Stopped with nothing in the player, else playing or paused at the
/// position.
// TODO: souvlaki reports `Rate` as 1.0 whatever the speed, so a widget
// that extrapolates the position drifts until the next update.
pub fn playback(now: Option<&NowPlaying>) -> MediaPlayback {
    let Some(now) = now else {
        return MediaPlayback::Stopped;
    };
    let progress = duration(now.position_ms).map(MediaPosition);
    if now.playing {
        MediaPlayback::Playing { progress }
    } else {
        MediaPlayback::Paused { progress }
    }
}

fn duration(ms: i64) -> Option<Duration> {
    u64::try_from(ms).ok().map(Duration::from_millis)
}

// ==============================================================================
// The OS to the player
// ==============================================================================

/// A media key or a request from the OS widget, as a player command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Remote {
    Toggle,
    Play,
    Pause,
    /// Next and Previous: a podcast has no track list, so they skip by the
    /// synced skip intervals, like the player bar's buttons.
    Skip {
        forward: bool,
    },
    /// Relative, in milliseconds.
    SeekBy(i64),
    /// Absolute, in milliseconds.
    SeekTo(i64),
}

/// The command for `event`, or `None` for requests Pod ignores (volume,
/// opening a URI, raising or quitting the app).
pub fn remote(event: MediaControlEvent) -> Option<Remote> {
    let millis = |duration: Duration| i64::try_from(duration.as_millis()).unwrap_or(i64::MAX);
    Some(match event {
        MediaControlEvent::Toggle => Remote::Toggle,
        MediaControlEvent::Play => Remote::Play,
        // Stopping a podcast keeps it in the player, where it was.
        MediaControlEvent::Pause | MediaControlEvent::Stop => Remote::Pause,
        MediaControlEvent::Next | MediaControlEvent::Seek(SeekDirection::Forward) => {
            Remote::Skip { forward: true }
        }
        MediaControlEvent::Previous | MediaControlEvent::Seek(SeekDirection::Backward) => {
            Remote::Skip { forward: false }
        }
        MediaControlEvent::SeekBy(SeekDirection::Forward, by) => Remote::SeekBy(millis(by)),
        MediaControlEvent::SeekBy(SeekDirection::Backward, by) => Remote::SeekBy(-millis(by)),
        MediaControlEvent::SetPosition(MediaPosition(to)) => Remote::SeekTo(millis(to)),
        MediaControlEvent::SetVolume(_)
        | MediaControlEvent::OpenUri(_)
        | MediaControlEvent::Raise
        | MediaControlEvent::Quit => return None,
    })
}

/// Runs `remote` on the player.
pub fn apply(player: &mut Player, remote: Remote, cx: &mut Context<Player>) {
    let Some(current) = player.current() else {
        return;
    };
    let (playing, position_ms) = (current.is_playing(), current.position_ms());
    match remote {
        Remote::Toggle => player.toggle(cx),
        Remote::Play if !playing => player.toggle(cx),
        Remote::Pause if playing => player.pause(cx),
        Remote::Play | Remote::Pause => {}
        Remote::Skip { forward } => player.skip(forward, cx),
        Remote::SeekBy(by) => player.seek(position_ms.saturating_add(by), cx),
        Remote::SeekTo(to) => player.seek(to, cx),
    }
}

// ==============================================================================
// The souvlaki boundary
// ==============================================================================

/// Pod's entry in the OS media controls, and what it shows.
pub struct OsControls {
    controls: MediaControls,
    /// What the controls show, so [`OsControls::show`] sends only changes.
    /// `None` is souvlaki's initial state: stopped, without metadata.
    shown: Option<NowPlaying>,
}

impl std::fmt::Debug for OsControls {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OsControls")
            .field("shown", &self.shown)
            .finish_non_exhaustive()
    }
}

impl OsControls {
    /// Registers with the OS, on Linux as `org.mpris.MediaPlayer2.pod` on
    /// the session bus. Blocks on D-Bus, so it runs off the UI thread.
    /// The OS's requests arrive on `remotes`, from souvlaki's thread.
    pub fn attach(remotes: mpsc::UnboundedSender<Remote>) -> Result<Self> {
        let mut controls = MediaControls::new(PlatformConfig {
            display_name: "Pod",
            dbus_name: DBUS_NAME,
            hwnd: None,
        })
        .map_err(|error| eyre!("{error}"))
        .context("create the media controls")?;
        controls
            .attach(move |event| {
                if let Some(remote) = remote(event) {
                    let _ = remotes.send(remote);
                }
            })
            .map_err(|error| eyre!("{error}"))
            .context("register with the OS media controls")?;
        Ok(Self {
            controls,
            shown: None,
        })
    }

    /// Shows `now`, sending souvlaki only what changed.
    pub fn show(&mut self, now: Option<NowPlaying>) -> Result<()> {
        let metadata = now.as_ref().map(NowPlaying::metadata);
        if self.shown.as_ref().map(NowPlaying::metadata) != metadata {
            self.controls
                .set_metadata(metadata.unwrap_or_default())
                .map_err(|error| eyre!("{error}"))
                .context("send the now-playing metadata")?;
        }
        let next = playback(now.as_ref());
        if playback(self.shown.as_ref()) != next {
            self.controls
                .set_playback(next)
                .map_err(|error| eyre!("{error}"))
                .context("send the playback state")?;
        }
        self.shown = now;
        Ok(())
    }
}

// ==============================================================================
// Wiring
// ==============================================================================

/// Keeps the [`OsControls`] in step with the player.
#[derive(Debug)]
pub struct MediaKeys {
    player: Entity<Player>,
    artwork_root: PathBuf,
    /// `None` until registered, and for good without a session bus.
    controls: Option<OsControls>,
    _player: Subscription,
    _attach: Task<()>,
    _remotes: Task<()>,
}

struct Installed(
    #[expect(dead_code, reason = "holds the entity for the app's lifetime")] Entity<MediaKeys>,
);

impl Global for Installed {}

/// Registers Pod with the OS media controls, off the UI thread, and keeps
/// them in step with `player`. Without them the app runs on and logs why.
pub fn install(player: &Entity<Player>, cx: &mut App) {
    let keys = cx.new(|cx| MediaKeys::new(player.clone(), cx));
    cx.set_global(Installed(keys));
}

impl MediaKeys {
    fn new(player: Entity<Player>, cx: &mut Context<Self>) -> Self {
        let (remotes, mut received) = mpsc::unbounded_channel();
        let attaching = cx
            .background_executor()
            .spawn(async move { OsControls::attach(remotes) });
        let attach = cx.spawn(async move |this, cx| match attaching.await {
            Ok(controls) => {
                let _ = this.update(cx, |this, cx| {
                    this.controls = Some(controls);
                    this.follow(cx);
                });
            }
            Err(error) => tracing::warn!("no media keys or now-playing widget: {error:#}"),
        });
        let target = player.downgrade();
        let remotes = cx.spawn(async move |_, cx| {
            while let Some(remote) = received.recv().await {
                if target
                    .update(cx, |player, cx| apply(player, remote, cx))
                    .is_err()
                {
                    return;
                }
            }
        });
        Self {
            _player: cx.observe(&player, |this, _, cx| this.follow(cx)),
            player,
            artwork_root: cx.global::<Reconciler>().artwork_root().to_owned(),
            controls: None,
            _attach: attach,
            _remotes: remotes,
        }
    }

    fn follow(&mut self, cx: &mut Context<Self>) {
        let Some(controls) = &mut self.controls else {
            return;
        };
        let now = now_playing(self.player.read(cx).current(), &self.artwork_root);
        if let Err(error) = controls.show(now) {
            tracing::warn!("stopped updating the now-playing widget: {error:#}");
            self.controls = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use pod_protocol::{EpisodeId, PodcastId};

    use super::*;
    use crate::player::contract::{Contract, Stored};
    use crate::player::{Episode, Source};

    const EPISODE: &str = "4b1e2c3d-4e5f-5a6b-9c7d-8e9f0a1b2c01";
    const PODCAST: &str = "9c3e5b1a-2f4d-5e6c-8a7b-1d2e3f4a5b6c";
    pub(super) fn root() -> &'static Path {
        Path::new("/home/ada/.local/share/pod/artwork")
    }

    fn episode() -> Episode {
        Episode {
            id: EPISODE.parse::<EpisodeId>().expect("the id is a UUID"),
            podcast_id: PODCAST.parse::<PodcastId>().expect("the id is a UUID"),
            title: "EP 151: The pager".to_owned(),
            podcast_title: "Darknet Diaries".to_owned(),
            artwork_url: Some("https://example.com/art.jpg".to_owned()),
            artwork_file: None,
            duration_ms: Some(3_120_000),
            source: Source::Stream("https://example.com/pager.mp3".to_owned()),
            stored: Stored {
                position_ms: 754_321,
                played: false,
            },
        }
    }

    /// The episode restored on start: in the player, not loaded into mpv.
    pub(super) fn parked() -> Current {
        Current {
            parked_ms: 754_321,
            episode: episode(),
            session: None,
        }
    }

    /// The episode playing, with mpv's duration and position.
    fn playing() -> Current {
        let episode = episode();
        let (mut session, _) =
            Contract::start(episode.stored, episode.duration_ms, 60_000, Instant::now());
        session.set_duration(3_118_500);
        session.progress(758_999, Instant::now());
        Current {
            parked_ms: 754_321,
            episode,
            session: Some(session),
        }
    }

    #[test]
    fn nothing_in_the_player_shows_nothing_and_stopped() {
        assert_eq!(now_playing(None, root()), None);
        assert_eq!(playback(None), MediaPlayback::Stopped);
    }

    #[test]
    fn the_metadata_names_the_episode_and_its_show() {
        let now = now_playing(Some(&playing()), root()).expect("an episode is in the player");
        assert_eq!(
            now.metadata(),
            MediaMetadata {
                title: Some("EP 151: The pager"),
                album: Some("Darknet Diaries"),
                artist: Some("Darknet Diaries"),
                cover_url: Some("https://example.com/art.jpg"),
                duration: Some(Duration::from_millis(3_118_500)),
            },
            "mpv's duration beats the feed's"
        );

        let mut cached = parked();
        cached.episode.artwork_file = Some("1f0e.jpg".to_owned());
        let now = now_playing(
            Some(&cached),
            Path::new("/home/ada/.local/share/pod 2/artwork"),
        )
        .expect("an episode is in the player");
        assert_eq!(
            now.metadata().cover_url,
            Some("file:///home/ada/.local/share/pod%202/artwork/1f0e.jpg"),
            "the cached file beats the remote URL"
        );

        let mut unknown = parked();
        unknown.episode.artwork_url = None;
        unknown.episode.duration_ms = None;
        let now = now_playing(Some(&unknown), root()).expect("an episode is in the player");
        assert_eq!(now.metadata().cover_url, None);
        assert_eq!(now.metadata().duration, None);
    }

    #[test]
    fn playback_follows_the_player() {
        let position = |seconds| Some(MediaPosition(Duration::from_secs(seconds)));

        let now = now_playing(Some(&playing()), root());
        assert_eq!(
            playback(now.as_ref()),
            MediaPlayback::Playing {
                progress: position(755)
            },
            "while playing, in steps of five seconds"
        );

        let now = now_playing(Some(&parked()), root());
        assert_eq!(
            playback(now.as_ref()),
            MediaPlayback::Paused {
                progress: position(754)
            },
            "a restored episode is paused where it was left"
        );

        let mut paused = playing();
        paused
            .session
            .as_mut()
            .expect("the episode is loaded")
            .pause(Instant::now());
        let now = now_playing(Some(&paused), root());
        assert_eq!(
            playback(now.as_ref()),
            MediaPlayback::Paused {
                progress: position(758)
            }
        );
    }

    #[test]
    fn progress_reaches_the_os_every_five_seconds() {
        let mut current = playing();
        let before = now_playing(Some(&current), root());
        let mut progress = |position_ms| {
            current
                .session
                .as_mut()
                .expect("the episode is loaded")
                .progress(position_ms, Instant::now());
            now_playing(Some(&current), root())
        };
        assert_eq!(progress(759_999), before);
        assert_ne!(progress(760_000), before);
    }

    #[test]
    fn os_requests_map_to_player_commands() {
        let cases = [
            (MediaControlEvent::Toggle, Some(Remote::Toggle)),
            (MediaControlEvent::Play, Some(Remote::Play)),
            (MediaControlEvent::Pause, Some(Remote::Pause)),
            (MediaControlEvent::Stop, Some(Remote::Pause)),
            (
                MediaControlEvent::Next,
                Some(Remote::Skip { forward: true }),
            ),
            (
                MediaControlEvent::Previous,
                Some(Remote::Skip { forward: false }),
            ),
            (
                MediaControlEvent::Seek(SeekDirection::Forward),
                Some(Remote::Skip { forward: true }),
            ),
            (
                MediaControlEvent::Seek(SeekDirection::Backward),
                Some(Remote::Skip { forward: false }),
            ),
            (
                MediaControlEvent::SeekBy(SeekDirection::Forward, Duration::from_secs(10)),
                Some(Remote::SeekBy(10_000)),
            ),
            (
                MediaControlEvent::SeekBy(SeekDirection::Backward, Duration::from_millis(5_500)),
                Some(Remote::SeekBy(-5_500)),
            ),
            (
                MediaControlEvent::SetPosition(MediaPosition(Duration::from_secs(60))),
                Some(Remote::SeekTo(60_000)),
            ),
            (MediaControlEvent::SetVolume(0.5), None),
            (MediaControlEvent::OpenUri("file:///a.mp3".to_owned()), None),
            (MediaControlEvent::Raise, None),
            (MediaControlEvent::Quit, None),
        ];
        for (event, expected) in cases {
            assert_eq!(remote(event.clone()), expected, "{event:?}");
        }
    }
}

/// MPRIS on a private bus, from a client's side, the way `playerctl` and
/// the desktop's widget see it.
#[cfg(all(test, target_os = "linux"))]
mod mpris_tests {
    use std::collections::HashMap;
    use std::io::{Read as _, Write as _};
    use std::process::{Child, Command, Stdio};
    use std::time::Instant;

    use zbus::zvariant::OwnedValue;

    use super::tests::{parked, root};
    use super::*;
    use crate::test_bus::{connect, run_on_private_bus};

    /// Set in the process that registers the controls.
    const CONTROLS_PROCESS: &str = "POD_TEST_MPRIS_CONTROLS";
    /// Bounds the whole test, blocking calls included.
    const TEST_DEADLINE: Duration = Duration::from_secs(30);
    /// Bounds each wait for the bus or the other process.
    const STEP_DEADLINE: Duration = Duration::from_secs(10);
    /// Shorter than [`STEP_DEADLINE`], so the controls process reports its
    /// own failure before the test gives up on it.
    const CONTROLS_DEADLINE: Duration = Duration::from_secs(7);

    #[zbus::proxy(
        interface = "org.mpris.MediaPlayer2.Player",
        default_service = "org.mpris.MediaPlayer2.pod",
        default_path = "/org/mpris/MediaPlayer2",
        gen_blocking = false
    )]
    trait MprisPlayer {
        fn play_pause(&self) -> zbus::Result<()>;

        #[zbus(property(emits_changed_signal = "false"))]
        fn metadata(&self) -> zbus::Result<HashMap<String, OwnedValue>>;

        #[zbus(property(emits_changed_signal = "false"))]
        fn playback_status(&self) -> zbus::Result<String>;

        #[zbus(property(emits_changed_signal = "false"))]
        fn position(&self) -> zbus::Result<i64>;
    }

    /// The controls' process, killed if the test gives up on it.
    struct ControlsProcess(Child);

    impl ControlsProcess {
        /// Stops the process and returns its exit status and output.
        fn finish(&mut self) -> String {
            let _ = self.0.kill();
            let status = self.0.wait();
            let mut output = format!("{status:?}\n");
            if let Some(mut stdout) = self.0.stdout.take() {
                let _ = stdout.read_to_string(&mut output);
            }
            if let Some(mut stderr) = self.0.stderr.take() {
                let _ = stderr.read_to_string(&mut output);
            }
            output
        }
    }

    impl Drop for ControlsProcess {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    // libdbus finds the session bus through `DBUS_SESSION_BUS_ADDRESS`, and
    // setting it here would race other tests' threads reading the
    // environment. So the controls run in a second process of this test
    // binary (`the_controls_process`), started with the private bus as its
    // session bus.
    #[test]
    fn mpris_shows_the_episode_and_play_pause_reaches_the_player() {
        run_on_private_bus(TEST_DEADLINE, |address| async move {
            let mut controls = ControlsProcess(
                Command::new(std::env::current_exe().expect("the test binary has a path"))
                    .args([
                        "player::media_keys::mpris_tests::the_controls_process",
                        "--exact",
                        "--ignored",
                    ])
                    .env("DBUS_SESSION_BUS_ADDRESS", &address)
                    .env(CONTROLS_PROCESS, "1")
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .expect("the test binary runs"),
            );
            let connection = connect(&address)
                .build()
                .await
                .expect("the client connects to the private bus");
            // A proxy built before the name exists keeps failing, so wait
            // for the controls to claim it first.
            let bus = zbus::fdo::DBusProxy::new(&connection)
                .await
                .expect("the bus proxy builds");
            let name =
                zbus::names::BusName::try_from(format!("org.mpris.MediaPlayer2.{DBUS_NAME}"))
                    .expect("the name is valid");
            let deadline = Instant::now() + STEP_DEADLINE;
            while !bus
                .name_has_owner(name.clone())
                .await
                .expect("the bus answers NameHasOwner")
            {
                if Instant::now() >= deadline {
                    panic!("the controls never registered: {}", controls.finish());
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            let player = MprisPlayerProxy::builder(&connection)
                .cache_properties(zbus::proxy::CacheProperties::No)
                .build()
                .await
                .expect("the proxy builds");

            let deadline = Instant::now() + STEP_DEADLINE;
            let metadata = loop {
                let metadata = player.metadata().await;
                if let Ok(metadata) = &metadata
                    && metadata.contains_key("xesam:title")
                {
                    break metadata.clone();
                }
                if Instant::now() >= deadline {
                    panic!(
                        "the episode never showed, last read {metadata:?}\ncontrols process: {}",
                        controls.finish()
                    );
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            };
            let text =
                |key: &str| String::try_from(metadata[key].clone()).expect("the value is a string");
            assert_eq!(text("xesam:title"), "EP 151: The pager");
            assert_eq!(text("mpris:artUrl"), "https://example.com/art.jpg");
            assert_eq!(
                Vec::<String>::try_from(metadata["xesam:artist"].clone())
                    .expect("the artists are strings"),
                ["Darknet Diaries"]
            );
            assert_eq!(
                i64::try_from(metadata["mpris:length"].clone()).expect("the length is an x"),
                3_120_000_000,
                "microseconds"
            );
            assert_eq!(
                player.playback_status().await.expect("the status reads"),
                "Paused"
            );
            assert_eq!(
                player.position().await.expect("the position reads"),
                754_000_000
            );

            player.play_pause().await.expect("PlayPause is answered");
            let deadline = Instant::now() + STEP_DEADLINE;
            loop {
                match controls.0.try_wait().expect("the process can be polled") {
                    Some(status) if status.success() => break,
                    Some(_) => panic!("the controls process failed: {}", controls.finish()),
                    None if Instant::now() >= deadline => {
                        panic!("Toggle never arrived: {}", controls.finish())
                    }
                    None => tokio::time::sleep(Duration::from_millis(20)).await,
                }
            }
        });
    }

    /// Shows the restored episode and exits once a Toggle arrives. Passes
    /// trivially when run on its own.
    #[test]
    #[ignore = "runs as the controls process of the test above"]
    fn the_controls_process() {
        if std::env::var_os(CONTROLS_PROCESS).is_none() {
            return;
        }
        // Straight to stderr, past libtest's capture, so the other side's
        // failure message shows how far this process got.
        let progress = |step: &str| {
            let _ = writeln!(std::io::stderr(), "controls process: {step}");
        };
        let (remotes, mut received) = mpsc::unbounded_channel();
        let mut controls = OsControls::attach(remotes).expect("the private bus takes the name");
        progress("registered");
        controls
            .show(now_playing(Some(&parked()), root()))
            .expect("the controls' thread runs");
        progress("showed the episode");
        let received = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime builds in tests")
            .block_on(async { tokio::time::timeout(CONTROLS_DEADLINE, received.recv()).await });
        if received != Ok(Some(Remote::Toggle)) {
            // Joins souvlaki's thread, whose error is otherwise lost.
            let thread = controls.controls.detach();
            panic!("received {received:?}, souvlaki's thread ended with {thread:?}");
        }
    }
}
