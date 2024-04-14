use bark_protocol::types::TimestampMicros;
use rustix::time::ClockId;

pub fn now() -> TimestampMicros {
    let timespec = rustix::time::clock_gettime(ClockId::Boottime);

    let micros = u64::try_from(timespec.tv_nsec / 1000)
        .expect("cannot convert i64 time value to u64");

    TimestampMicros(micros)
}
