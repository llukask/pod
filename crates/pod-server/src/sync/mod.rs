//! `POST /v1/sync` domain logic (spec section 4.2 to 4.4).
//!
//! A sync request pushes first and pulls second, in two transactions: the
//! push applies the client's mutations under the user-scope lock
//! ([`push::apply_batch`]), then the pull reads every requested scope from
//! one snapshot.
// TODO(KIL-20): add the pull half as `pull.rs` next to `push.rs`.

pub mod push;
mod subscription;

pub use push::apply_batch;
