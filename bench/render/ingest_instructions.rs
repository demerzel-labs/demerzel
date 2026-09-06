//! Standalone instruction counter for the historical step-7 ingest comparison.
//! Copy this file unchanged into each revision's TUI examples directory.
//! Build both with the same compiler and `--release --locked`.

#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
compile_error!("This instruction counter requires Linux x86_64.");

use std::ffi::{c_int, c_long, c_ulong};
use std::fs::File;
use std::hint::black_box;
use std::io::{self, Read};
use std::os::fd::{AsRawFd, FromRawFd};
use std::sync::atomic::{Ordering, compiler_fence};

use demerzel_tui::bench::{MAX_BLOCK_BYTES, Screen};

const STREAM_BYTES: usize = 64 * 1024;
const DELTA_BYTES: usize = 16;
const HISTORY_LINES: usize = 200;
const WARMUPS: usize = 10;
const SAMPLES: usize = 31;

// Linux x86_64 ABI: perf_event_open and PERF_EVENT_IOC_*.
const SYS_PERF_EVENT_OPEN: c_long = 298;
const ENABLE: c_ulong = 0x2400;
const DISABLE: c_ulong = 0x2401;
const RESET: c_ulong = 0x2403;

unsafe extern "C" {
    fn syscall(number: c_long, ...) -> c_long;
    fn ioctl(fd: c_int, request: c_ulong, ...) -> c_int;
}

// PERF_ATTR_SIZE_VER0. No fields added after this 64-byte ABI are needed.
#[repr(C)]
struct PerfEventAttr {
    event_type: u32,
    size: u32,
    config: u64,
    sample_period: u64,
    sample_type: u64,
    read_format: u64,
    flags: u64,
    wakeup_events: u32,
    bp_type: u32,
    config1: u64,
}

struct Counter(File);

#[derive(Clone, Copy)]
struct Sample {
    instructions: u64,
    time_enabled_ns: u64,
    time_running_ns: u64,
}

impl Counter {
    fn open() -> io::Result<Self> {
        let attr = PerfEventAttr {
            event_type: 0, // PERF_TYPE_HARDWARE
            size: size_of::<PerfEventAttr>() as u32,
            config: 1, // PERF_COUNT_HW_INSTRUCTIONS
            sample_period: 0,
            sample_type: 0,
            read_format: 3, // TOTAL_TIME_ENABLED | TOTAL_TIME_RUNNING
            // disabled | pinned | exclude_kernel | exclude_hv
            flags: (1 << 0) | (1 << 2) | (1 << 5) | (1 << 6),
            wakeup_events: 0,
            bp_type: 0,
            config1: 0,
        };
        // Count this thread on any CPU. Close the descriptor on exec.
        // SAFETY: attr has the documented layout and lives through the call.
        let fd = unsafe {
            syscall(
                SYS_PERF_EVENT_OPEN,
                &attr as *const PerfEventAttr,
                0 as c_int,
                -1 as c_int,
                -1 as c_int,
                8 as c_ulong, // PERF_FLAG_FD_CLOEXEC
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the syscall returned a new descriptor owned by this counter.
        Ok(Self(unsafe { File::from_raw_fd(fd as c_int) }))
    }

    fn control(&self, request: c_ulong) -> io::Result<()> {
        // SAFETY: the descriptor is open; these requests take no pointer.
        if unsafe { ioctl(self.0.as_raw_fd(), request, 0 as c_ulong) } < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    fn read(&mut self) -> io::Result<Sample> {
        let mut bytes = [0; 24];
        self.0.read_exact(&mut bytes)?;
        Ok(Sample {
            instructions: u64::from_ne_bytes(bytes[0..8].try_into().unwrap()),
            time_enabled_ns: u64::from_ne_bytes(bytes[8..16].try_into().unwrap()),
            time_running_ns: u64::from_ne_bytes(bytes[16..24].try_into().unwrap()),
        })
    }

    fn measure<T>(&mut self, work: impl FnOnce() -> T) -> io::Result<(T, Sample)> {
        // RESET clears the count but does not clear the cumulative times.
        let previous = self.read()?;
        self.control(RESET)?;
        self.control(ENABLE)?;
        compiler_fence(Ordering::SeqCst);
        let output = black_box(work());
        compiler_fence(Ordering::SeqCst);
        self.control(DISABLE)?;
        let mut sample = self.read()?;
        sample.time_enabled_ns = sample
            .time_enabled_ns
            .checked_sub(previous.time_enabled_ns)
            .ok_or_else(|| io::Error::other("The enabled time decreased."))?;
        sample.time_running_ns = sample
            .time_running_ns
            .checked_sub(previous.time_running_ns)
            .ok_or_else(|| io::Error::other("The running time decreased."))?;
        if sample.instructions == 0
            || sample.time_enabled_ns == 0
            || sample.time_enabled_ns != sample.time_running_ns
        {
            return Err(io::Error::other(
                "The counter did not run for the complete span. Reject this run.",
            ));
        }
        Ok((output, sample))
    }
}

fn history_line(index: usize) -> String {
    format!(
        "{index:05} {}",
        "lorem ipsum dolor sit ".repeat(1 + index % 7)
    )
}

fn stream_text() -> String {
    const WORDS: &str = "the quick brown fox jumps over the lazy dog ";
    let mut text = String::with_capacity(STREAM_BYTES + WORDS.len());
    let mut column = 0;
    while text.len() < STREAM_BYTES {
        text.push_str(WORDS);
        column += WORDS.len();
        if column >= 80 {
            text.push('\n');
            column = 0;
        }
    }
    text.truncate(STREAM_BYTES);
    assert!(text.is_ascii());
    text
}

fn screen_with_history() -> Screen {
    let mut screen = Screen::new(120, 40, false);
    for index in 0..HISTORY_LINES {
        screen.push_line(&history_line(index));
    }
    screen.draw();
    assert_eq!(screen.blocks(), HISTORY_LINES);
    screen
}

fn ingest(mut screen: Screen, deltas: &[&str]) -> Screen {
    for delta in deltas {
        screen.push_text(delta);
    }
    screen
}

fn print_samples(samples: &[Sample]) {
    print!("[");
    for (index, sample) in samples.iter().enumerate() {
        if index != 0 {
            print!(",");
        }
        print!(
            "{{\"instructions\":{},\"time_enabled_ns\":{},\"time_running_ns\":{}}}",
            sample.instructions, sample.time_enabled_ns, sample.time_running_ns
        );
    }
    print!("]");
}

fn run() -> io::Result<()> {
    let mut counter = Counter::open()?;
    let text = stream_text();
    let deltas: Vec<&str> = text
        .as_bytes()
        .chunks(DELTA_BYTES)
        .map(|chunk| std::str::from_utf8(chunk).expect("ASCII stream"))
        .collect();
    let mut samples = Vec::with_capacity(SAMPLES);
    let mut empty_samples = Vec::with_capacity(SAMPLES);
    for index in 0..WARMUPS + SAMPLES {
        let (_, empty) = counter.measure(|| black_box(()))?;
        let screen = screen_with_history();
        let (screen, sample) = counter.measure(|| ingest(black_box(screen), black_box(&deltas)))?;
        assert_eq!(MAX_BLOCK_BYTES, 32 * 1024);
        assert_eq!(screen.blocks(), HISTORY_LINES + 2);
        drop(screen);
        if index >= WARMUPS {
            samples.push(sample);
            empty_samples.push(empty);
        }
    }
    print!(
        "{{\"counter\":\"instructions:user\",\"scope\":\"calling thread\",\"stream_bytes\":{STREAM_BYTES},\"delta_bytes\":{DELTA_BYTES},\"history_blocks\":{HISTORY_LINES},\"screen\":[120,40],\"markdown\":false,\"warmups\":{WARMUPS},\"sample_count\":{SAMPLES},\"note\":\"Raw counts include counter control overhead and the owned screen transfer. Empty spans estimate control overhead; no subtraction is applied. Fixture setup, initial draw, assertions, and drop stay outside the counter.\",\"empty_samples\":"
    );
    print_samples(&empty_samples);
    print!(",\"samples\":");
    print_samples(&samples);
    println!("}}");
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("Instruction measurement failed: {error}");
        std::process::exit(1);
    }
}
