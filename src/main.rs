mod receive;
mod protocol;
mod time;
mod status;

use std::net::{UdpSocket, Ipv4Addr, SocketAddrV4};
use std::process::ExitCode;
use std::sync::{Mutex, Arc};
use std::time::Duration;

use bytemuck::Zeroable;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{OutputCallbackInfo, StreamConfig, InputCallbackInfo, BufferSize, SupportedBufferSize};
use structopt::StructOpt;

use protocol::{TimestampMicros, Packet, PacketBuffer};

use crate::time::{SampleDuration, Timestamp};

#[derive(StructOpt)]
enum Opt {
    Stream(StreamOpt),
    Receive(ReceiveOpt),
}

#[derive(StructOpt, Clone)]
struct ReceiveOpt {
    #[structopt(long, short)]
    pub group: Ipv4Addr,
    #[structopt(long, short)]
    pub port: u16,
    #[structopt(long, short)]
    pub bind: Option<Ipv4Addr>,
    #[structopt(long, default_value="12")]
    pub max_seq_gap: usize,
}

#[derive(StructOpt)]
struct StreamOpt {
    #[structopt(long, short)]
    pub group: Ipv4Addr,
    #[structopt(long, short)]
    pub port: u16,
    #[structopt(long, short)]
    pub bind: Option<SocketAddrV4>,
    #[structopt(long, default_value="20")]
    pub delay_ms: u64,
}

#[derive(Debug)]
enum RunError {
    BindSocket(SocketAddrV4, std::io::Error),
    JoinMulticast(std::io::Error),
    NoDeviceAvailable,
    NoSupportedStreamConfig,
    StreamConfigs(cpal::SupportedStreamConfigsError),
    BuildStream(cpal::BuildStreamError),
    Stream(cpal::PlayStreamError),
    Socket(std::io::Error),
}

fn main() -> Result<(), ExitCode> {
    let opt = Opt::from_args();

    let result = match opt {
        Opt::Stream(opt) => run_stream(opt),
        Opt::Receive(opt) => run_receive(opt),
    };

    result.map_err(|err| {
        eprintln!("error: {err:?}");
        ExitCode::FAILURE
    })
}

fn run_stream(opt: StreamOpt) -> Result<(), RunError> {
    let host = cpal::default_host();

    let device = host.default_input_device()
        .ok_or(RunError::NoDeviceAvailable)?;

    let config = config_for_device(&device)?;

    let bind = opt.bind.unwrap_or(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0));

    let socket = UdpSocket::bind(bind)
        .map_err(|e| RunError::BindSocket(bind, e))?;

    let delay = Duration::from_millis(opt.delay_ms);
    let delay = SampleDuration::from_std_duration_lossy(delay);

    let mut packet = Packet {
        magic: protocol::MAGIC,
        flags: 0,
        seq: 1,
        pts: TimestampMicros(0),
        buffer: PacketBuffer::zeroed(),
    };

    let mut packet_written = SampleDuration::zero();

    let stream = device.build_input_stream(&config,
        move |mut data: &[f32], _: &InputCallbackInfo| {
            // assert data only contains complete frames:
            assert!(data.len() % usize::from(protocol::CHANNELS) == 0);

            let mut timestamp = Timestamp::now().add(delay);

            if packet.pts.0 == 0 {
                packet.pts = timestamp.to_micros_lossy();
            }

            while data.len() > 0 {
                let buffer_offset = packet_written.as_buffer_offset();
                let buffer_remaining = packet.buffer.0.len() - buffer_offset;

                let copy_count = std::cmp::min(data.len(), buffer_remaining);
                let buffer_copy_end = buffer_offset + copy_count;

                packet.buffer.0[buffer_offset..buffer_copy_end]
                    .copy_from_slice(&data[0..copy_count]);

                data = &data[copy_count..];
                packet_written = SampleDuration::from_buffer_offset(buffer_copy_end);
                timestamp = timestamp.add(SampleDuration::from_buffer_offset(copy_count));

                if packet_written == SampleDuration::ONE_PACKET {
                    // packet is full! send:
                    let dest = SocketAddrV4::new(opt.group, opt.port);
                    socket.send_to(bytemuck::bytes_of(&packet), dest)
                        .expect("UdpSocket::send");

                    // reset rest of packet for next:
                    packet.seq += 1;
                    packet.pts = timestamp.to_micros_lossy();
                    packet_written = SampleDuration::zero();
                }
            }

            // if there is data waiting in the packet buffer at the end of the
            // callback, the pts we just calculated is valid. if the packet is
            // empty, reset the pts to 0. this signals the next callback to set
            // pts to the current time when it fires.
            if packet_written == SampleDuration::zero() {
                packet.pts.0 = 0;
            }
        },
        move |err| {
            eprintln!("stream error! {err:?}");
        },
        None
    ).map_err(RunError::BuildStream)?;

    stream.play().map_err(RunError::Stream)?;

    loop {
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn run_receive(opt: ReceiveOpt) -> Result<(), RunError> {
    let host = cpal::default_host();

    let device = host.default_output_device()
        .ok_or(RunError::NoDeviceAvailable)?;

    let config = config_for_device(&device)?;

    struct SharedState {
        pub recv: receive::Receiver,
    }

    let state = Arc::new(Mutex::new(SharedState {
        recv: receive::Receiver::new(receive::ReceiverOpt {
            max_seq_gap: opt.max_seq_gap,
        }),
    }));

    let _stream = device.build_output_stream(&config,
        {
            let state = state.clone();
            move |data: &mut [f32], info: &OutputCallbackInfo| {
                let stream_timestamp = info.timestamp();

                let output_latency = stream_timestamp.playback
                    .duration_since(&stream_timestamp.callback)
                    .unwrap_or_default();

                let output_latency = SampleDuration::from_std_duration_lossy(output_latency);

                let now = Timestamp::now();
                let pts = now.add(output_latency);

                let mut state = state.lock().unwrap();
                state.recv.fill_stream_buffer(data, pts);
            }
        },
        move |err| {
            eprintln!("stream error! {err:?}");
        },
        None
    ).map_err(RunError::BuildStream)?;

    let bind_ip = opt.bind.unwrap_or(Ipv4Addr::UNSPECIFIED);
    let bind_addr = SocketAddrV4::new(bind_ip, opt.port);

    let socket = UdpSocket::bind(bind_addr)
        .map_err(|e| RunError::BindSocket(bind_addr, e))?;

    socket.join_multicast_v4(&opt.group, &bind_ip)
        .map_err(RunError::JoinMulticast)?;

    loop {
        let mut packet = Packet::zeroed();

        let nread = socket.recv(bytemuck::bytes_of_mut(&mut packet))
            .map_err(RunError::Socket)?;

        if nread < std::mem::size_of::<Packet>() {
            eprintln!("packet wrong size! ignoring");
            continue;
        }

        let mut state = state.lock().unwrap();
        state.recv.push_packet(&packet);
    }
}

fn config_for_device(device: &cpal::Device) -> Result<StreamConfig, RunError> {
    let configs = device.supported_input_configs()
        .map_err(RunError::StreamConfigs)?;

    let config = configs
        .filter(|config| config.sample_format() == protocol::SAMPLE_FORMAT)
        .filter(|config| config.channels() == protocol::CHANNELS)
        .nth(0)
        .ok_or(RunError::NoSupportedStreamConfig)?;

    let buffer_size = match config.buffer_size() {
        SupportedBufferSize::Range { min, .. } => {
            std::cmp::max(*min, protocol::FRAMES_PER_PACKET as u32)
        }
        SupportedBufferSize::Unknown => {
            protocol::FRAMES_PER_PACKET as u32
        }
    };

    Ok(StreamConfig {
        channels: protocol::CHANNELS,
        sample_rate: protocol::SAMPLE_RATE,
        buffer_size: BufferSize::Fixed(buffer_size),
    })
}
