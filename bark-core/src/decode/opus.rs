use core::fmt::{self, Display};

use bark_protocol::SAMPLE_RATE;

use super::{Decode, DecodeError, SampleBuffer};

pub struct OpusDecoder {
    opus: opus::Decoder,
}

impl OpusDecoder {
    pub fn new() -> Result<Self, opus::Error> {
        let opus = opus::Decoder::new(
            SAMPLE_RATE.0,
            opus::Channels::Stereo,
        )?;

        Ok(OpusDecoder { opus })
    }
}

impl Display for OpusDecoder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "opus")
    }
}

impl Decode for OpusDecoder {
    fn decode_packet(&mut self, bytes: Option<&[u8]>, out: &mut SampleBuffer) -> Result<(), DecodeError> {
        let expected = out.len() / 2;

        let length = match bytes {
            Some(bytes) => self.opus.decode_float(bytes, out, false)?,
            None => self.opus.decode_float(&[], out, true)?,
        };

        if expected != length {
            return Err(DecodeError::WrongLength { length, expected });
        }

        Ok(())
    }
}
