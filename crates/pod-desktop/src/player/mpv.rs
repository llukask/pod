//! libmpv in audio-only mode, behind two channels.
//!
//! mpv's client API blocks: `wait_event` waits for the next event, and a
//! command or property write takes the core lock. Neither may run on the UI
//! thread, so [`start`] runs two threads per player. The command thread
//! takes [`Command`]s off a channel and calls mpv in order; the event
//! thread waits on mpv's event queue and forwards what the player needs as
//! [`Event`]s. The player only ever sends and receives on channels, which
//! is also how tests script it without mpv.
//!
//! Dropping the command sender quits mpv: the command thread sends `quit`,
//! the event thread sees the shutdown and exits, and the last of the two
//! to let go of the handle destroys it.

use std::sync::Arc;

use color_eyre::eyre::{Result, WrapErr, eyre};
use libmpv2::events::{Event as MpvEvent, PropertyData};
use libmpv2::{Format, Mpv, mpv_end_file_reason};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

/// What the player asks mpv to do. Positions are milliseconds.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// Replace the current file. `source` is a local path or a URL.
    Load {
        source: String,
        start_ms: i64,
        paused: bool,
    },
    Pause(bool),
    Seek(i64),
    Speed(f64),
    /// Unload the file, as sign-out clears the player.
    Stop,
}

/// What mpv reports back. Positions are milliseconds.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// A loaded file started. Events before it belong to the file it
    /// replaced.
    Started,
    Duration(i64),
    Position(i64),
    /// The file played to its end.
    Ended,
    /// The file couldn't be opened or broke off while playing.
    Failed(Failure),
}

/// Why mpv couldn't play a file, as the player bar tells the user. mpv's
/// own error code stays in the log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    /// mpv couldn't open the sound device.
    NoAudioOutput,
    /// The file or URL didn't open as audio: missing, a 404, or not a
    /// format mpv reads.
    Unloadable,
    Other,
}

impl From<&libmpv2::Error> for Failure {
    fn from(error: &libmpv2::Error) -> Self {
        use libmpv2::mpv_error;
        match error {
            libmpv2::Error::Raw(mpv_error::AoInitFailed) => Failure::NoAudioOutput,
            libmpv2::Error::Raw(
                mpv_error::LoadingFailed
                | mpv_error::NothingToPlay
                | mpv_error::UnknownFormat
                | mpv_error::Unsupported,
            ) => Failure::Unloadable,
            _ => Failure::Other,
        }
    }
}

/// The player's ends of the two channels.
#[derive(Debug)]
pub struct Engine {
    pub commands: UnboundedSender<Command>,
    pub events: UnboundedReceiver<Event>,
}

impl Engine {
    /// Channels with nothing behind them, for tests that play mpv's part:
    /// the returned ends receive the commands and send the events.
    #[cfg(test)]
    pub fn scripted() -> (Engine, UnboundedReceiver<Command>, UnboundedSender<Event>) {
        let (commands, command_receiver) = unbounded_channel();
        let (event_sender, events) = unbounded_channel();
        (Engine { commands, events }, command_receiver, event_sender)
    }
}

/// `reply_userdata` of the observed properties.
const DURATION: u64 = 1;
const TIME_POS: u64 = 2;

/// Starts mpv and its two threads. `audio_output` is mpv's `ao`; tests
/// pass `null` to play without a sound device.
pub fn start(audio_output: Option<&str>) -> Result<Engine> {
    let mpv = Mpv::with_initializer(|init| {
        // libmpv already reads no config files and has no terminal; these
        // make it play audio only and stay alive between files.
        init.set_option("vid", "no")?;
        init.set_option("audio-display", "no")?;
        init.set_option("idle", "yes")?;
        init.set_option("ytdl", "no")?;
        init.set_option("audio-client-name", "pod")?;
        if let Some(ao) = audio_output {
            init.set_option("ao", ao)?;
        }
        Ok(())
    })
    .map_err(|error| eyre!("{error}"))
    .context("create the mpv handle")?;
    mpv.disable_deprecated_events()
        .map_err(|error| eyre!("{error}"))
        .context("turn off mpv's deprecated events")?;
    for (name, id) in [("duration", DURATION), ("time-pos", TIME_POS)] {
        mpv.observe_property(name, Format::Double, id)
            .map_err(|error| eyre!("{error}"))
            .with_context(|| format!("observe mpv's {name}"))?;
    }
    let mpv = Arc::new(mpv);

    let (commands, command_receiver) = unbounded_channel();
    let (event_sender, events) = unbounded_channel();
    std::thread::Builder::new()
        .name("pod-mpv-commands".into())
        .spawn({
            let mpv = mpv.clone();
            move || run_commands(&mpv, command_receiver)
        })
        .context("spawn the mpv command thread")?;
    std::thread::Builder::new()
        .name("pod-mpv-events".into())
        .spawn(move || forward_events(&mpv, &event_sender))
        .context("spawn the mpv event thread")?;
    Ok(Engine { commands, events })
}

fn run_commands(mpv: &Mpv, mut commands: UnboundedReceiver<Command>) {
    while let Some(command) = commands.blocking_recv() {
        if let Err(error) = run_command(mpv, &command) {
            tracing::warn!(?command, "mpv command failed: {error}");
        }
    }
    if let Err(error) = mpv.command("quit", &[]) {
        tracing::warn!("quitting mpv failed: {error}");
    }
}

fn run_command(mpv: &Mpv, command: &Command) -> libmpv2::Result<()> {
    match command {
        Command::Load {
            source,
            start_ms,
            paused,
        } => {
            // `start` and `pause` set before `loadfile` apply to the file it
            // loads. Unlike loadfile's per-file options, this works the same
            // across mpv versions, which moved those options to a later
            // argument in 0.38.
            mpv.set_property("start", seconds(*start_ms).as_str())?;
            mpv.set_property("pause", *paused)?;
            mpv.command("loadfile", &[source, "replace"])
        }
        Command::Pause(paused) => mpv.set_property("pause", *paused),
        Command::Seek(position_ms) => {
            mpv.command("seek", &[&seconds(*position_ms), "absolute+exact"])
        }
        Command::Speed(speed) => mpv.set_property("speed", *speed),
        Command::Stop => mpv.command("stop", &[]),
    }
}

fn seconds(ms: i64) -> String {
    format!("{:.3}", ms as f64 / 1000.0)
}

fn forward_events(mpv: &Mpv, events: &UnboundedSender<Event>) {
    loop {
        // `None` is a timeout, or a property that became unavailable
        // between files, which the player doesn't need to hear about.
        let Some(event) = mpv.wait_event(-1.0) else {
            continue;
        };
        let forwarded = match event {
            Ok(MpvEvent::Shutdown) => return,
            Ok(MpvEvent::StartFile) => Event::Started,
            Ok(MpvEvent::EndFile(mpv_end_file_reason::Eof)) => Event::Ended,
            Ok(MpvEvent::PropertyChange {
                change: PropertyData::Double(seconds),
                reply_userdata,
                ..
            }) => {
                let ms = (seconds * 1000.0).round() as i64;
                match reply_userdata {
                    DURATION => Event::Duration(ms),
                    TIME_POS => Event::Position(ms),
                    _ => continue,
                }
            }
            // libmpv2 reports a file that ended in an error as an error
            // event; nothing else the player waits on fails this way.
            Err(error) => {
                tracing::warn!("mpv couldn't play the file: {error}");
                Event::Failed(Failure::from(&error))
            }
            Ok(_) => continue,
        };
        if events.send(forwarded).is_err() {
            // The player is gone; its command sender went with it, so mpv
            // is quitting.
            return;
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::path::Path;
    use std::time::Duration;

    use super::*;

    /// Writes `seconds` of 8 kHz mono silence as a WAV file.
    pub(crate) fn write_silence(path: &Path, seconds: u32) {
        let rate: u32 = 8000;
        let data_bytes = rate * seconds * 2;
        let mut wav = Vec::new();
        wav.extend(b"RIFF");
        wav.extend((36 + data_bytes).to_le_bytes());
        wav.extend(b"WAVEfmt ");
        wav.extend(16u32.to_le_bytes()); // fmt chunk size
        wav.extend(1u16.to_le_bytes()); // PCM
        wav.extend(1u16.to_le_bytes()); // mono
        wav.extend(rate.to_le_bytes());
        wav.extend((rate * 2).to_le_bytes()); // bytes per second
        wav.extend(2u16.to_le_bytes()); // bytes per frame
        wav.extend(16u16.to_le_bytes()); // bits per sample
        wav.extend(b"data");
        wav.extend(data_bytes.to_le_bytes());
        wav.resize(wav.len() + data_bytes as usize, 0);
        std::fs::write(path, wav).expect("the scratch directory is writable");
    }

    async fn next(events: &mut UnboundedReceiver<Event>) -> Event {
        tokio::time::timeout(Duration::from_secs(10), events.recv())
            .await
            .expect("mpv reports within ten seconds")
            .expect("the event thread runs while the engine lives")
    }

    /// Plays a real file through libmpv with the null audio output.
    #[tokio::test]
    async fn mpv_plays_a_file_from_the_start_position_to_its_end() {
        let dir = tempfile::tempdir().expect("a scratch directory");
        let file = dir.path().join("silence.wav");
        write_silence(&file, 2);
        let mut engine = start(Some("null")).expect("libmpv starts");

        engine
            .commands
            .send(Command::Load {
                source: file.display().to_string(),
                start_ms: 1_200,
                paused: false,
            })
            .expect("the command thread runs");
        assert_eq!(next(&mut engine.events).await, Event::Started);

        let mut positions = Vec::new();
        let mut duration = None;
        loop {
            match next(&mut engine.events).await {
                Event::Duration(ms) => duration = Some(ms),
                Event::Position(ms) => positions.push(ms),
                Event::Ended => break,
                other => panic!("unexpected {other:?}"),
            }
        }
        assert_eq!(duration, Some(2_000));
        let first = *positions.first().expect("mpv reports positions");
        assert!(
            (1_200..1_500).contains(&first),
            "playback starts at the start position, got {first}"
        );
        assert!(positions.is_sorted(), "{positions:?}");
    }

    #[tokio::test]
    async fn mpv_seeks_and_pauses_on_command() {
        let dir = tempfile::tempdir().expect("a scratch directory");
        let file = dir.path().join("silence.wav");
        write_silence(&file, 30);
        let mut engine = start(Some("null")).expect("libmpv starts");
        let send = |command| {
            engine
                .commands
                .send(command)
                .expect("the command thread runs");
        };

        send(Command::Load {
            source: file.display().to_string(),
            start_ms: 0,
            paused: true,
        });
        assert_eq!(next(&mut engine.events).await, Event::Started);
        // mpv can't seek until the file is loaded, which its first
        // position report follows.
        while !matches!(next(&mut engine.events).await, Event::Position(_)) {}
        send(Command::Seek(20_000));
        let reached = loop {
            if let Event::Position(ms) = next(&mut engine.events).await
                && ms >= 20_000
            {
                break ms;
            }
        };
        assert!(reached < 20_500, "paused, the position stays at the seek");
        send(Command::Speed(2.0));
        send(Command::Pause(false));
        let played_on = loop {
            if let Event::Position(ms) = next(&mut engine.events).await
                && ms > 20_500
            {
                break ms;
            }
        };
        assert!(played_on > 20_500);
        send(Command::Load {
            source: dir.path().join("missing.wav").display().to_string(),
            start_ms: 0,
            paused: false,
        });
        let failure = loop {
            if let Event::Failed(failure) = next(&mut engine.events).await {
                break failure;
            }
        };
        assert_eq!(failure, Failure::Unloadable);
    }
}
