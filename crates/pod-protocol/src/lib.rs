//! Wire types shared by the Pod server and its Rust clients.
//!
//! This crate stays free of I/O dependencies so every client can embed it
//! (spec section 11).

// TODO: add the sync wire types, id derivation and merge function (spec
// sections 1, 4 and 5.4) once the server core needs them.
