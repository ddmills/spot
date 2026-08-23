//! Internet radio: the station directory and the player that streams one.
//!
//! The directory and the audio path have nothing to do with Spotify. See
//! [`player`] for why radio needs an audio path of its own, and [`api`] for why
//! the directory is Radio Browser. [`track`] is the one place the two
//! catalogues meet: it reads what a station announces and works out which
//! Spotify record that is.

pub mod api;
pub mod player;
pub mod track;
