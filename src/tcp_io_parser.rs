// Architectural design:
//   * Using `Decoder` trait we decode SQL command into `String`
//   * Using `Encoder` trait we encode Received Result<OutputTable, T: Display>
//     Typically, generic T is `Error`, which then converted using `ToString` trait

use crate::sql::OutputTable;
use derive_more::Display;
use rmp_serde::encode::Error as RMPError;
use serde::Serialize;
use std::fmt;
use tokio_util::bytes::{Buf, BufMut, BytesMut};
use tokio_util::codec::{Decoder, Encoder};

type HeaderType = u64;
const HEADER_SIZE: usize = size_of::<HeaderType>();

// Created for derive Display and IO error handling (required by `Encoder` and `Decoder` traits).
#[derive(Debug, Serialize, Display)]
pub enum ProtocolError {
    #[display("SQL parsing error. Unknown length")]
    UnknownLength,
    #[display("SQL parsing error. Invalid data model: {_0}")]
    InvalidDataModel(String),
    #[display("SQL parsing error. Syntax error: {_0}")]
    Syntax(String),
    #[display("SQL parsing error. Depth limit exceeded")]
    DepthLimitExceeded,
    #[display("SQL parsing error. IO error: {_0}")]
    IOError(String),

    #[display("Conversion error. {_0}")]
    Conversion(String),
}

// Required by `Encoder` and `Decoder` traits.
impl From<std::io::Error> for ProtocolError {
    fn from(error: std::io::Error) -> Self {
        Self::IOError(error.to_string())
    }
}

impl From<RMPError> for ProtocolError {
    fn from(error: RMPError) -> Self {
        match error {
            RMPError::InvalidValueWrite(s) => Self::InvalidDataModel(s.to_string()),
            RMPError::UnknownLength => Self::UnknownLength,
            RMPError::InvalidDataModel(s) => Self::InvalidDataModel(s.to_string()),
            RMPError::DepthLimitExceeded => Self::DepthLimitExceeded,
            RMPError::Syntax(s) => Self::Syntax(s),
        }
    }
}

/// TCP protocol parser implementing `tokio_util::codec::{Decoder, Encoder}` traits.
///
/// Protocol format:
/// - Header: 8-byte little-endian u64 containing body size
/// - Body: UTF-8 encoded SQL command (for decoding) or `MessagePack` response (for encoding)
pub struct Parser;

impl Decoder for Parser {
    type Item = String;
    type Error = ProtocolError;

    fn decode(&mut self, buf: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        if buf.len() < HEADER_SIZE {
            return Ok(None);
        }

        // Peek at the header without consuming it
        let mut header_bytes = [0; HEADER_SIZE];
        for i in 0..HEADER_SIZE {
            header_bytes[i] = buf[i];
        }

        let body_size = usize::try_from(HeaderType::from_le_bytes(header_bytes))
            .map_err(|_| ProtocolError::Conversion("Header type too large".to_string()))?;
        let total_message_size = HEADER_SIZE + body_size;

        if buf.len() < total_message_size {
            buf.reserve(total_message_size - buf.len());
            return Ok(None);
        }

        // Now consume the header and the data
        buf.advance(HEADER_SIZE);
        let data = buf.split_to(body_size);
        let decoded = String::from_utf8_lossy(&data).into_owned();

        Ok(Some(decoded))
    }
}

impl<T> Encoder<Result<OutputTable, T>> for Parser
where
    T: fmt::Display,
{
    type Error = ProtocolError;

    fn encode(
        &mut self,
        item: Result<OutputTable, T>,
        buf: &mut BytesMut,
    ) -> Result<(), Self::Error> {
        let item = item.map_err(|error| error.to_string());
        let command_bytes = rmp_serde::to_vec(&item)?;

        let message_size = (command_bytes.len() as HeaderType).to_le_bytes();
        buf.put(message_size.as_slice()); // HEADER
        buf.put(command_bytes.as_slice()); // BODY

        Ok(())
    }
}
