use flexi_logger::detailed_format;
use flexi_logger::writers::LogWriter;

use std::cell::UnsafeCell;
use std::fs::File;
use std::io::Write;
use std::os::unix::io::FromRawFd;

pub struct StdoutWriter {
    file: UnsafeCell<File>,
}

unsafe impl Sync for StdoutWriter {}

impl StdoutWriter {
    pub fn new() -> StdoutWriter {
        let file = unsafe { UnsafeCell::new(File::from_raw_fd(1)) };

        // TODO: fix memory leak here
        StdoutWriter { file }
    }
}

impl LogWriter for StdoutWriter {
    fn write(
        &self,
        now: &mut flexi_logger::DeferredNow,
        record: &flexi_logger::Record,
    ) -> std::io::Result<()> {
        unsafe {
            let file = &mut *self.file.get();
            detailed_format(file, now, record)?;
            file.write_all(b"\n")
        }
    }

    fn flush(&self) -> std::io::Result<()> {
        unsafe { (*self.file.get()).flush() }
    }

    fn max_log_level(&self) -> flexi_logger::LevelFilter {
        // TODO: configurable log level
        flexi_logger::LevelFilter::Error
    }
}
