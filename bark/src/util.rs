use cpal::{BufferSize, SampleFormat, StreamConfig, SupportedBufferSize, SupportedStreamConfigRange};
use cpal::traits::DeviceTrait;

use crate::RunError;

pub const SAMPLE_FORMAT: SampleFormat = SampleFormat::F32;

pub fn make_stream_config(configs: Box<dyn Iterator<Item = SupportedStreamConfigRange>>) -> Result<StreamConfig, RunError> {
    let config = configs
        .filter(|config| config.sample_format() == SAMPLE_FORMAT)
        // .filter(|config| config.channels() == bark_protocol::CHANNELS.0)
        .nth(0)
        .ok_or(RunError::NoSupportedStreamConfig)?;

    let buffer_size = match config.buffer_size() {
        SupportedBufferSize::Range { min, .. } => {
            std::cmp::max(min, &(bark_protocol::FRAMES_PER_PACKET as u32))
        }
        SupportedBufferSize::Unknown => {
            &(bark_protocol::FRAMES_PER_PACKET as u32)
        }
    };

    Ok(StreamConfig {
        channels: bark_protocol::CHANNELS.0,
        sample_rate: cpal::SampleRate(bark_protocol::SAMPLE_RATE.0),
        buffer_size: BufferSize::Fixed(*buffer_size),
    })
}
