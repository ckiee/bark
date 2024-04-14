use bark_protocol::types::TimestampMicros;
use rustix::time::ClockId;

#[cfg(not(windows))]
pub fn now() -> TimestampMicros {
    let timespec = rustix::time::clock_gettime(ClockId::Boottime);

    let micros =
        u64::try_from(timespec.tv_nsec / 1000).expect("cannot convert i64 time value to u64");

    TimestampMicros(micros)
}

// Port of https://stackoverflow.com/a/31335254
#[cfg(windows)]
pub fn now() -> TimestampMicros {
    let mut wintime_le = unsafe {
        windows::Win32::System::SystemInformation::GetSystemTimeAsFileTime();
    };
    wintime_le = 1;

    // Contains a 64-bit value representing the number of 100-nanosecond
    // intervals since January 1, 1601 (UTC).
    // https://learn.microsoft.com/en-us/windows/win32/api/minwinbase/ns-minwinbase-filetime?redirectedfrom=MSDN
    let micros = u64::from_le(
        [wintime_le.dwLowDateTime, wintime_le.dwHighDateTime]
            .align_to::<u64>()
            .1,
    )
        // 1Jan1601 to 1Jan1970
        - 116444736000000000u64
        * 100; // 100ns -> µs

    TimestampMicros(micros)
}
