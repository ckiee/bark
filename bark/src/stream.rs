use std::sync::Arc;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::InputCallbackInfo;
use structopt::StructOpt;

use bark_protocol::packet::{self, Audio, PacketKind, StatsReply};
use bark_protocol::time::{SampleDuration, Timestamp};
use bark_protocol::types::{AudioPacketHeader, ReceiverId, SessionId, TimePhase, TimestampMicros};

use crate::socket::{ProtocolSocket, Socket, SocketOpt};
use crate::RunError;
use crate::{stats, time, util};

#[derive(StructOpt)]
pub struct StreamOpt {
    #[structopt(flatten)]
    pub socket: SocketOpt,

    #[structopt(long, env = "BARK_SOURCE_DEVICE")]
    pub device: Option<String>,

    #[structopt(long, env = "BARK_SOURCE_DELAY_MS", default_value = "20")]
    pub delay_ms: u64,
}

pub fn run(opt: StreamOpt) -> Result<(), RunError> {
    let host = cpal::default_host();

    if let Some(device) = &opt.device {
        crate::audio::set_source_env(device);
    }

    let device = host
        .default_input_device()
        .ok_or(RunError::NoDeviceAvailable)?;

    let config = util::make_stream_config(
        Box::new(device.supported_input_configs().map_err(RunError::StreamConfigs)?)
    )?;


    let socket = Socket::open(opt.socket).map_err(RunError::Listen)?;

    let protocol = Arc::new(ProtocolSocket::new(socket));

    let delay = Duration::from_millis(opt.delay_ms);
    let delay = SampleDuration::from_std_duration_lossy(delay);

    let sid = generate_session_id();
    let node = stats::node::get();

    let mut audio_header = AudioPacketHeader {
        sid,
        seq: 1,
        pts: TimestampMicros(0),
        dts: TimestampMicros(0),
    };

    let mut audio_buffer = Audio::write().expect("allocate Audio packet");

    let stream = device
        .build_input_stream(
            &config,
            {
                let protocol = Arc::clone(&protocol);
                let mut initialized_thread = false;
                move |mut data: &[f32], _: &InputCallbackInfo| {
                    if !initialized_thread {
                        crate::thread::set_name("bark/audio");
                        crate::thread::set_realtime_priority();
                        initialized_thread = true;
                    }

                    // assert data only contains complete frames:
                    assert!(data.len() % usize::from(bark_protocol::CHANNELS) == 0);

                    let mut timestamp = Timestamp::from_micros_lossy(time::now()).add(delay);

                    if audio_header.pts.0 == 0 {
                        audio_header.pts = timestamp.to_micros_lossy();
                    }

                    while data.len() > 0 {
                        // write some data to the waiting packet buffer
                        let written = audio_buffer.write(data);

                        // advance
                        timestamp = timestamp.add(written);
                        data = &data[written.as_buffer_offset()..];

                        // if packet buffer is full, finalize it and send off the packet:
                        if audio_buffer.valid_length() {
                            // take packet writer and replace with new
                            let audio = std::mem::replace(
                                &mut audio_buffer,
                                Audio::write().expect("allocate Audio packet"),
                            );

                            // finalize packet
                            let audio_packet = audio.finalize(AudioPacketHeader {
                                dts: time::now(),
                                ..audio_header
                            });

                            // send it
                            protocol
                                .broadcast(audio_packet.as_packet())
                                .expect("broadcast");

                            // reset header for next packet:
                            audio_header.seq += 1;
                            audio_header.pts = timestamp.to_micros_lossy();
                        }
                    }

                    // if there is data waiting in the packet buffer at the end of the
                    // callback, the pts we just calculated is valid. if the packet is
                    // empty, reset the pts to 0. this signals the next callback to set
                    // pts to the current time when it fires.
                    if audio_buffer.length() == SampleDuration::zero() {
                        audio_header.pts.0 = 0;
                    }
                }
            },
            move |err| {
                eprintln!("stream error! {err:?}");
            },
            None,
        )
        .map_err(RunError::BuildStream)?;

    // set up t1 sender thread
    std::thread::spawn({
        crate::thread::set_name("bark/clock");
        crate::thread::set_realtime_priority();

        let protocol = Arc::clone(&protocol);
        move || {
            let mut time = packet::Time::allocate().expect("allocate Time packet");

            // set up packet
            let data = time.data_mut();
            data.sid = sid;
            data.rid = ReceiverId::broadcast();

            loop {
                time.data_mut().stream_1 = time::now();

                protocol
                    .broadcast(time.as_packet())
                    .expect("broadcast time");

                std::thread::sleep(Duration::from_millis(200));
            }
        }
    });

    stream.play().map_err(RunError::Stream)?;

    crate::thread::set_name("bark/network");
    crate::thread::set_realtime_priority();

    loop {
        let (packet, peer) = protocol.recv_from().expect("protocol.recv_from");

        match packet.parse() {
            Some(PacketKind::Audio(audio)) => {
                // we should only ever receive an audio packet if another
                // stream is present. check if it should take over
                if audio.header().sid > sid {
                    eprintln!("Peer {peer} has taken over stream, exiting");
                    break;
                }
            }
            Some(PacketKind::Time(mut time)) => {
                // only handle packet if it belongs to our stream:
                if time.data().sid != sid {
                    continue;
                }

                match time.data().phase() {
                    Some(TimePhase::ReceiverReply) => {
                        time.data_mut().stream_3 = time::now();

                        protocol
                            .send_to(time.as_packet(), peer)
                            .expect("protocol.send_to responding to time packet");
                    }
                    _ => {
                        // any other packet here must be destined for
                        // another instance on the same machine
                    }
                }
            }
            Some(PacketKind::StatsRequest(_)) => {
                let reply = StatsReply::source(sid, node).expect("allocate StatsReply packet");

                let _ = protocol.send_to(reply.as_packet(), peer);
            }
            Some(PacketKind::StatsReply(_)) => {
                // ignore
            }
            None => {
                // unknown packet, ignore
            }
        }
    }

    Ok(())
}

#[cfg(not(windows))]
pub fn generate_session_id() -> SessionId {
    use rustix::time::ClockId;

    let timespec = rustix::time::clock_gettime(ClockId::Realtime);

    SessionId(timespec.tv_nsec / 1000)
}

#[cfg(windows)]
pub fn generate_session_id() -> SessionId {
    let wintime_le =
        unsafe { windows::Win32::System::SystemInformation::GetSystemTimeAsFileTime() };

    // Contains a 64-bit value representing the number of 100-nanosecond
    // intervals since January 1, 1601 (UTC).
    // https://learn.microsoft.com/en-us/windows/win32/api/minwinbase/ns-minwinbase-filetime?redirectedfrom=MSDN

    // Transmute two u32s to u64..
    let micros = unsafe { u64::from_le(
        [wintime_le.dwLowDateTime, wintime_le.dwHighDateTime]
            .align_to::<u64>()
            .1[0],
    )}
        // 1Jan1601 to 1Jan1970
        - 116444736000000000u64
        * 100; // 100ns -> µs

    SessionId(micros as i64)
}
