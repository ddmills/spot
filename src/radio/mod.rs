//! Internet radio: the station directory and the player that streams one.
//!
//! Nothing in here touches Spotify. See [`player`] for why radio needs an
//! audio path of its own, and [`api`] for why the directory is Radio Browser.

pub mod api;
pub mod player;
