use bark_protocol::types::TimestampMicros;

#[cfg(not(windows))]
pub fn now() -> TimestampMicros {
    use rustix::time::ClockId;
    let timespec = rustix::time::clock_gettime(ClockId::Boottime);

    let micros =
        u64::try_from(timespec.tv_nsec / 1000).expect("cannot convert i64 time value to u64");

    TimestampMicros(micros)
}

// Port of https://stackoverflow.com/a/31335254
#[cfg(windows)]
pub fn now() -> TimestampMicros {
    let millis: u64 = unsafe { windows::Win32::System::SystemInformation::GetTickCount64() };

    TimestampMicros(millis * 1000)
}
