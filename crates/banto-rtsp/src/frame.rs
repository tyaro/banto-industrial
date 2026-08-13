//! Boundary-safe JPEG frame extraction from an arbitrary byte stream.

use std::fmt;

use crate::RtspError;

/// A decoded JPEG payload with its sequence and receive timestamp.
#[derive(Clone, PartialEq, Eq)]
pub struct VideoFrame {
    pub sequence: u64,
    pub received_at: std::time::SystemTime,
    pub jpeg: Vec<u8>,
}

impl fmt::Debug for VideoFrame {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VideoFrame")
            .field("sequence", &self.sequence)
            .field("received_at", &self.received_at)
            .field("jpeg_bytes", &self.jpeg.len())
            .finish()
    }
}

impl VideoFrame {
    pub fn new(sequence: u64, received_at: std::time::SystemTime, jpeg: Vec<u8>) -> Self {
        Self {
            sequence,
            received_at,
            jpeg,
        }
    }
}

/// Extracts SOI/EOI-delimited JPEG frames, preferring recovery over retaining
/// an unbounded or corrupted frame buffer.
pub struct JpegFrameDecoder {
    max_frame_bytes: usize,
    current_frame: Vec<u8>,
    scan_ff: bool,
}

impl JpegFrameDecoder {
    /// Creates a decoder. Four bytes is the smallest possible SOI+EOI frame.
    pub fn new(max_frame_bytes: usize) -> Result<Self, RtspError> {
        if max_frame_bytes < 4 {
            return Err(crate::RtspConfigError::InvalidFrameLimit.into());
        }
        Ok(Self {
            max_frame_bytes,
            current_frame: Vec::new(),
            scan_ff: false,
        })
    }

    pub const fn max_frame_bytes(&self) -> usize {
        self.max_frame_bytes
    }

    /// Pushes arbitrary stream bytes and returns every complete JPEG found.
    /// On an over-limit frame the decoder resets itself, so the next push can
    /// start a fresh frame without reconstructing the decoder.
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<Vec<u8>>, RtspError> {
        let mut frames = Vec::new();
        for &byte in chunk {
            if self.current_frame.is_empty() {
                self.scan_for_soi(byte);
                continue;
            }

            self.current_frame.push(byte);
            let has_nested_soi = self.current_frame.len() >= 4
                && self.current_frame[self.current_frame.len() - 2..] == [0xff, 0xd8];
            if has_nested_soi {
                self.current_frame.clear();
                self.current_frame.extend_from_slice(&[0xff, 0xd8]);
                self.scan_ff = false;
                continue;
            }

            if self.current_frame.len() > self.max_frame_bytes {
                self.reset();
                return Err(RtspError::FrameTooLarge {
                    max_frame_bytes: self.max_frame_bytes,
                });
            }

            if self.current_frame.len() >= 4
                && self.current_frame[self.current_frame.len() - 2..] == [0xff, 0xd9]
            {
                frames.push(std::mem::take(&mut self.current_frame));
                self.scan_ff = false;
            }
        }
        Ok(frames)
    }

    fn scan_for_soi(&mut self, byte: u8) {
        if self.scan_ff && byte == 0xd8 {
            self.current_frame.extend_from_slice(&[0xff, 0xd8]);
            self.scan_ff = false;
        } else {
            self.scan_ff = byte == 0xff;
        }
    }

    fn reset(&mut self) {
        self.current_frame.clear();
        self.scan_ff = false;
    }
}

impl fmt::Debug for JpegFrameDecoder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("JpegFrameDecoder")
            .field("max_frame_bytes", &self.max_frame_bytes)
            .field("buffered_bytes", &self.current_frame.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decoder(limit: usize) -> JpegFrameDecoder {
        JpegFrameDecoder::new(limit).unwrap()
    }

    #[test]
    fn extracts_single_frame() {
        let mut decoder = decoder(32);
        assert_eq!(
            decoder.push(&[0xff, 0xd8, 1, 2, 0xff, 0xd9]).unwrap(),
            vec![vec![0xff, 0xd8, 1, 2, 0xff, 0xd9]]
        );
    }

    #[test]
    fn handles_marker_split_across_chunks() {
        let mut decoder = decoder(32);
        assert!(decoder.push(&[0xff]).unwrap().is_empty());
        assert!(decoder.push(&[0xd8, 1, 2, 0xff]).unwrap().is_empty());
        assert_eq!(
            decoder.push(&[0xd9]).unwrap(),
            vec![vec![0xff, 0xd8, 1, 2, 0xff, 0xd9]]
        );
    }

    #[test]
    fn drops_noise_and_returns_multiple_frames() {
        let mut decoder = decoder(32);
        let stream = [
            0, 1, 2, 0xff, 0xd8, 3, 0xff, 0xd9, 9, 0xff, 0xd8, 4, 0xff, 0xd9,
        ];
        assert_eq!(
            decoder.push(&stream).unwrap(),
            vec![
                vec![0xff, 0xd8, 3, 0xff, 0xd9],
                vec![0xff, 0xd8, 4, 0xff, 0xd9]
            ]
        );
    }

    #[test]
    fn resynchronizes_to_nested_soi() {
        let mut decoder = decoder(32);
        let stream = [0xff, 0xd8, 1, 0xff, 0xd8, 2, 0xff, 0xd9];
        assert_eq!(
            decoder.push(&stream).unwrap(),
            vec![vec![0xff, 0xd8, 2, 0xff, 0xd9]]
        );
    }

    #[test]
    fn over_limit_is_structured_and_next_push_recovers() {
        let mut decoder = decoder(6);
        let error = decoder.push(&[0xff, 0xd8, 1, 2, 3, 4, 5]).unwrap_err();
        assert_eq!(error, RtspError::FrameTooLarge { max_frame_bytes: 6 });
        assert_eq!(
            decoder.push(&[0xff, 0xd8, 0xff, 0xd9]).unwrap(),
            vec![vec![0xff, 0xd8, 0xff, 0xd9]]
        );
    }

    #[test]
    fn accepts_smallest_limit_and_rejects_smaller_limits() {
        assert!(JpegFrameDecoder::new(3).is_err());
        let mut decoder = decoder(4);
        assert_eq!(
            decoder.push(&[0xff, 0xd8, 0xff, 0xd9]).unwrap(),
            vec![vec![0xff, 0xd8, 0xff, 0xd9]]
        );
    }
}
