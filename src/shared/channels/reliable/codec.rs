use std::{
    fmt,
    io::{self, Cursor},
};

use bytes::{Buf, BufMut, Bytes, BytesMut};
use tokio_util::codec::{Decoder, Encoder};

use crate::shared::channels::PROTOCOL_HEADER_LEN;

use super::{RELIABLE_FRAME_LENGTH_FIELD_LEN, RELIABLE_FRAME_TOTAL_HEADER_LEN};

#[derive(Debug, Clone, Copy)]
enum DecodeState {
    Head,
    Data(usize),
}

/// An error when the number of bytes read is more than max frame length.
pub struct QuinnetProtocolCodecError {
    _priv: (),
}

impl fmt::Debug for QuinnetProtocolCodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("QuinnetProtocolCodecError").finish()
    }
}

impl fmt::Display for QuinnetProtocolCodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("frame size too big")
    }
}

impl std::error::Error for QuinnetProtocolCodecError {}

#[derive(Debug)]
pub struct QuinnetProtocolCodecEncoder {
    // Maximum frame length
    max_frame_len: usize,
    raw_channel_id: u8,
}

impl QuinnetProtocolCodecEncoder {
    pub fn new(raw_channel_id: u8, max_frame_len: usize) -> Self {
        Self {
            raw_channel_id,
            max_frame_len,
        }
    }
}

impl Encoder<Bytes> for QuinnetProtocolCodecEncoder {
    type Error = io::Error;

    fn encode(&mut self, frame: Bytes, dst: &mut BytesMut) -> Result<(), io::Error> {
        if frame.len() > self.max_frame_len {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                QuinnetProtocolCodecError { _priv: () },
            ));
        }

        // Reserve capacity in the destination buffer to fit the frame and header fields.
        dst.reserve(RELIABLE_FRAME_TOTAL_HEADER_LEN + frame.len());
        // Header
        dst.put_uint(
            PROTOCOL_HEADER_LEN as u64 + frame.len() as u64,
            RELIABLE_FRAME_LENGTH_FIELD_LEN,
        );
        dst.put_u8(self.raw_channel_id);

        // Write the frame to the buffer
        dst.extend_from_slice(&frame[..]);

        Ok(())
    }
}

#[derive(Debug)]
pub struct QuinnetProtocolCodecDecoder {
    // Read state
    state: DecodeState,
    // Maximum frame length
    max_frame_len: usize,
}

impl QuinnetProtocolCodecDecoder {
    pub fn new(max_frame_len: usize) -> Self {
        Self {
            max_frame_len,
            state: DecodeState::Head,
        }
    }

    fn decode_head(&mut self, src: &mut BytesMut) -> io::Result<Option<usize>> {
        if src.len() < RELIABLE_FRAME_TOTAL_HEADER_LEN {
            // Not enough data
            return Ok(None);
        }

        let payload_length = {
            let mut src = Cursor::new(&mut *src);

            let payload_length = src.get_uint(RELIABLE_FRAME_LENGTH_FIELD_LEN);

            if payload_length > self.max_frame_len as u64 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    QuinnetProtocolCodecError { _priv: () },
                ));
            }

            // The check above ensures there is no overflow
            payload_length as usize
        };

        // src.advance(self.builder.get_num_skip());
        src.advance(RELIABLE_FRAME_LENGTH_FIELD_LEN);

        // Ensure that the buffer has enough space to read the incoming
        // payload
        src.reserve(payload_length.saturating_sub(src.len()));

        Ok(Some(payload_length))
    }

    fn decode_data(&self, payload_length: usize, src: &mut BytesMut) -> Option<BytesMut> {
        // At this point, the buffer has already had the required capacity
        // reserved. All there is to do is read.
        if src.len() < payload_length {
            return None;
        }

        Some(src.split_to(payload_length))
    }
}

impl Decoder for QuinnetProtocolCodecDecoder {
    type Item = BytesMut;
    type Error = io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> io::Result<Option<BytesMut>> {
        let payload_length = match self.state {
            DecodeState::Head => match self.decode_head(src)? {
                Some(payload_length) => {
                    self.state = DecodeState::Data(payload_length);
                    payload_length
                }
                None => return Ok(None),
            },
            DecodeState::Data(payload_length) => payload_length,
        };

        match self.decode_data(payload_length, src) {
            Some(data) => {
                // Update the decode state
                self.state = DecodeState::Head;

                // Make sure the buffer has enough space to read the next head
                src.reserve(RELIABLE_FRAME_TOTAL_HEADER_LEN.saturating_sub(src.len()));

                Ok(Some(data))
            }
            None => Ok(None),
        }
    }
}
