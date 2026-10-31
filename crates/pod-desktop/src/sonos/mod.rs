//! Sonos groups as an output of the player (spec 13.4): the `/v1/sonos`
//! requests ([`api`]) and the pure model of a session handed to a group
//! ([`remote`]).
//!
//! The output is part of the player's state, not an entity of its own: the
//! player bar, the episode rows and the media keys all read the [`Player`],
//! so a session on a group is one more phase of it
//! (`player::Phase::Remote`), and `player/output.rs` wires the requests
//! and the polling into it.
//!
//! [`Player`]: crate::player::Player

pub mod api;
pub mod remote;
#[cfg(test)]
mod tests;

pub use api::{Api, SonosError};
pub use remote::{Remote, Target};
