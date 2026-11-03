//! What flows from the tap's ring onward, shared by the core and the lite:
//! the segment as an encoder or a wire reads it and the ring's bytes made
//! into it (`feed`), and the conformer that holds a chosen rate and channel
//! count across the player's changes or follows the player (`conform`).

pub mod conform;
pub mod feed;
