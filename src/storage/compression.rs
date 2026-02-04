use crate::error::{Error, Result};
use crate::storage::ValueType;
use rkyv::{Archive as RkyvArchive, Deserialize as RkyvDeserialize, Serialize as RkyvSerialize};
use serde::Serialize;
use std::io::{Read as _, Write as _};

#[derive(Debug, Clone, PartialEq, Serialize, RkyvArchive, RkyvSerialize, RkyvDeserialize)]
pub enum CompressionType {
    None,
    LZ4(u8),
}

impl Default for CompressionType {
    fn default() -> Self {
        Self::LZ4(3)
    }
}

impl ValueType {
    // add other compressions in future, when either own parser is written or sqlparser-rs allows to choose compression during table creation
    #[allow(clippy::unused_self)]
    pub fn get_optimal_compression(&self) -> CompressionType {
        CompressionType::LZ4(3)
    }
}

/// Compresses bytes using the specified compression type.
///
/// Returns:
///   * Ok: Compressed bytes.
///   * Error: `CouldNotInsertData` on compression failure.
pub fn compress_bytes(bytes: &[u8], compression_type: &CompressionType) -> Result<Vec<u8>> {
    match *compression_type {
        CompressionType::LZ4(level) => {
            let output = Vec::with_capacity(bytes.len() / 2); // on average compresses 2x
            let mut encoder = lz4::EncoderBuilder::new()
                .level(u32::from(level))
                .checksum(lz4::ContentChecksum::NoChecksum)
                .build(output)
                .map_err(|_| Error::CouldNotInsertData("Could not compress data.".to_string()))?;
            encoder
                .write_all(bytes)
                .map_err(|_| Error::CouldNotInsertData("Could not compress data.".to_string()))?;
            let (output, _compression) = encoder.finish();
            Ok(output)
        }
        CompressionType::None => Ok(bytes.to_vec()),
    }
}

/// Decompresses bytes using the specified compression type.
///
/// Returns:
///   * Ok: Decompressed bytes.
///   * Error: `CouldNotReadData` on decompression failure.
pub fn decompress_bytes(
    compressed_bytes: &[u8],
    compression_type: &CompressionType,
) -> Result<Vec<u8>> {
    match compression_type {
        CompressionType::LZ4(_) => {
            let mut decoder = lz4::Decoder::new(compressed_bytes).map_err(|error| {
                Error::CouldNotReadData(format!("Failed to create LZ4 decoder: {error}"))
            })?;
            let mut decompressed = Vec::with_capacity(compressed_bytes.len() * 2); // on average decompresses 2x
            decoder.read_to_end(&mut decompressed).map_err(|error| {
                Error::CouldNotReadData(format!("Failed to decompress LZ4 data: {error}",))
            })?;
            Ok(decompressed)
        }
        CompressionType::None => Ok(compressed_bytes.to_vec()),
    }
}

#[cfg(test)]
mod tests {
    use super::{CompressionType, compress_bytes, decompress_bytes};

    #[test]
    fn compress_decompress() {
        let bytes = b"Hello world";
        let compression_type = CompressionType::LZ4(3);
        assert_eq!(
            decompress_bytes(
                &compress_bytes(bytes, &compression_type).unwrap(),
                &compression_type
            )
            .unwrap(),
            bytes
        );
    }
}
