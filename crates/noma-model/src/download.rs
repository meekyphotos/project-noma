//! A reader that reports how much of the model has arrived.

use std::io::Read;

use crate::Progress;

/// How often to call back. Half a megabyte is often enough for a smooth bar
/// and rare enough that the HUD is not woken thousands of times.
const REPORT_EVERY: u64 = 512 * 1024;

pub struct ProgressReader<'a, R: Read> {
    inner: R,
    received: u64,
    total: u64,
    reported: u64,
    on_progress: &'a mut dyn FnMut(Progress),
}

impl<'a, R: Read> ProgressReader<'a, R> {
    pub fn new(inner: R, total: u64, on_progress: &'a mut dyn FnMut(Progress)) -> Self {
        on_progress(Progress::Downloading { received: 0, total });
        Self {
            inner,
            received: 0,
            total,
            reported: 0,
            on_progress,
        }
    }
}

impl<R: Read> Read for ProgressReader<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let read = self.inner.read(buf)?;
        self.received += read as u64;
        let finished = read == 0;
        if finished || self.received - self.reported >= REPORT_EVERY {
            self.reported = self.received;
            (self.on_progress)(Progress::Downloading {
                received: self.received,
                total: self.total.max(self.received),
            });
        }
        Ok(read)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hands out `chunk` bytes at a time so we can watch the callbacks.
    struct Chunked {
        remaining: usize,
        chunk: usize,
    }

    impl Read for Chunked {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let count = self.chunk.min(buf.len()).min(self.remaining);
            self.remaining -= count;
            Ok(count)
        }
    }

    fn collect(total_bytes: usize, declared: u64) -> Vec<Progress> {
        let mut seen = Vec::new();
        {
            let mut record = |progress: Progress| seen.push(progress);
            let source = Chunked {
                remaining: total_bytes,
                chunk: REPORT_EVERY as usize,
            };
            let mut reader = ProgressReader::new(source, declared, &mut record);
            let mut sink = std::io::sink();
            std::io::copy(&mut reader, &mut sink).expect("copy");
        }
        seen
    }

    #[test]
    fn reports_a_start_and_a_finish() {
        let seen = collect(REPORT_EVERY as usize * 3, REPORT_EVERY * 3);
        assert_eq!(
            seen.first(),
            Some(&Progress::Downloading {
                received: 0,
                total: REPORT_EVERY * 3
            })
        );
        assert_eq!(
            seen.last(),
            Some(&Progress::Downloading {
                received: REPORT_EVERY * 3,
                total: REPORT_EVERY * 3
            })
        );
    }

    #[test]
    fn progress_never_goes_backwards() {
        let seen = collect(REPORT_EVERY as usize * 4, REPORT_EVERY * 4);
        let received: Vec<u64> = seen
            .iter()
            .map(|progress| match progress {
                Progress::Downloading { received, .. } => *received,
                Progress::Extracting => unreachable!(),
            })
            .collect();
        assert!(received.windows(2).all(|pair| pair[0] <= pair[1]));
    }

    #[test]
    fn a_wrong_content_length_never_exceeds_a_hundred_percent() {
        // Server under-reports: the bar must not run past the end.
        let seen = collect(REPORT_EVERY as usize * 3, 10);
        assert!(seen.iter().all(|progress| progress.percent() <= 90.0));
    }
}
