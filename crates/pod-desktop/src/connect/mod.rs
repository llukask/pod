//! Connect (spec 14): one playback per account, seen and controlled from
//! every signed-in device, and moved between them and Sonos groups.
//!
//! - [`stream`] holds the device's event stream on the tokio side, with the
//!   server-sent events parsed by [`sse`].
//! - [`api`] sends reports, commands and transfers, and lists the Sonos
//!   groups for the output picker.
//! - [`remote`] is the pure part: where the playback is relative to this
//!   device, what a command does to the local player, and another device's
//!   playback as the bar shows it.
//!
//! Connect is part of the player's state, not an entity of its own: the
//! player bar, the episode rows and the media keys all read the [`Player`],
//! so another device's playback is one more phase of it
//! (`player::Phase::Remote`), and `player/output.rs` wires the stream, the
//! reports and the requests into it.
//!
//! [`Player`]: crate::player::Player

pub mod api;
pub mod remote;
pub mod sse;
pub mod stream;
#[cfg(test)]
mod tests;

pub use api::{Api, ConnectError};
pub use remote::{Remote, Target};
