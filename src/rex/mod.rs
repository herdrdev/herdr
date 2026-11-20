pub(crate) mod client;
pub(crate) mod data;

pub(crate) use client::{RexControl, RexError, RexEvent, MIN_API_REVISION, TARGET_API_REVISION};
pub(crate) use data::{RexData, RexDataError, StreamItem, TERMINAL_SUBPROTOCOL};
