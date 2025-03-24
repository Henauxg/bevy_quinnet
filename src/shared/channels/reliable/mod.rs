use super::PROTOCOL_HEADER_LEN;

pub(crate) mod codec;
pub(crate) mod recv;
pub(crate) mod send;

/// Default max frame length for payloads sent on reliable channels, in bytes
pub const DEFAULT_MAX_RELIABLE_FRAME_LEN: usize = 8 * 1_024 * 1_024;

// PAYLOAD LENGTH | CHANNEL ID | PAYLOAD
pub(crate) const RELIABLE_FRAME_LENGTH_FIELD_LEN: usize = 4;
pub(crate) const RELIABLE_FRAME_TOTAL_HEADER_LEN: usize =
    RELIABLE_FRAME_LENGTH_FIELD_LEN + PROTOCOL_HEADER_LEN;
