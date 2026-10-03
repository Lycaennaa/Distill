use std::io::{self, BufRead, BufReader, Read};
use std::os::fd::AsFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

#[cfg(unix)]
use nix::poll::{PollFd, PollFlags, poll};

use super::model::CaptureSinks;
use crate::digest::{Digest, OutputLine, Stream};

pub(super) const EVENT_CHANNEL_CAPACITY: usize = 128;
const MAX_LINE_BYTES: usize = 65_536;
const MAX_STRUCTURED_LINE_BYTES: usize = 4_194_304;

#[derive(Debug)]
pub(super) enum ReaderEvent {
    Raw(Stream, Vec<u8>),
    Line(OutputLine),
    Finished(Stream),
    Failed(Stream),
}

pub(super) fn spawn_reader<R>(
    stream: Stream,
    reader: R,
    sender: SyncSender<ReaderEvent>,
    stop: Arc<AtomicBool>,
    capture_raw: bool,
) -> JoinHandle<()>
where
    R: Read + AsFd + Send + 'static,
{
    thread::spawn(move || {
        let mut reader = BufReader::new(reader);
        let mut ordinal = 0_u64;
        let mut accumulator = LineAccumulator::default();
        loop {
            match read_chunk(&mut reader, &stop) {
                Ok(Some(chunk)) => {
                    if capture_raw
                        && !send_event(&sender, &stop, ReaderEvent::Raw(stream, chunk.clone()))
                    {
                        return;
                    }
                    for line in accumulator.push(&chunk) {
                        let mut text = String::from_utf8_lossy(&line.bytes).into_owned();
                        if line.truncated {
                            if line.structured {
                                text.push_str(" …[structured diagnostic truncated]");
                            } else {
                                text.push_str(" …[truncated]");
                            }
                        }
                        let output = OutputLine::new(stream, ordinal, trim_line_end(text));
                        ordinal = ordinal.saturating_add(1);
                        if !send_event(&sender, &stop, ReaderEvent::Line(output)) {
                            return;
                        }
                    }
                }
                Ok(None) => {
                    if let Some(line) = accumulator.finish() {
                        let mut text = String::from_utf8_lossy(&line.bytes).into_owned();
                        if line.truncated {
                            if line.structured {
                                text.push_str(" …[structured diagnostic truncated]");
                            } else {
                                text.push_str(" …[truncated]");
                            }
                        }
                        let output = OutputLine::new(stream, ordinal, trim_line_end(text));
                        if !send_event(&sender, &stop, ReaderEvent::Line(output)) {
                            return;
                        }
                    }
                    break;
                }
                Err(_error) => {
                    if !send_event(&sender, &stop, ReaderEvent::Failed(stream)) {
                        return;
                    }
                    return;
                }
            }
        }
        send_event(&sender, &stop, ReaderEvent::Finished(stream));
    })
}

fn send_event(sender: &SyncSender<ReaderEvent>, stop: &AtomicBool, mut event: ReaderEvent) -> bool {
    loop {
        match sender.try_send(event) {
            Ok(()) => return true,
            Err(TrySendError::Full(next)) => {
                if stop.load(Ordering::Relaxed) {
                    return false;
                }
                event = next;
                thread::sleep(Duration::from_millis(1));
            }
            Err(TrySendError::Disconnected(_event)) => return false,
        }
    }
}

fn wait_until_readable<R: AsFd>(reader: &BufReader<R>, stop: &AtomicBool) -> io::Result<bool> {
    #[cfg(unix)]
    {
        loop {
            if !reader.buffer().is_empty() {
                return Ok(true);
            }
            if stop.load(Ordering::Relaxed) {
                return Ok(false);
            }

            let mut descriptors = [PollFd::new(
                reader.get_ref().as_fd(),
                PollFlags::POLLIN | PollFlags::POLLHUP | PollFlags::POLLERR,
            )];
            match poll(&mut descriptors, 50_u16) {
                Ok(_) => {
                    if !reader.buffer().is_empty() {
                        return Ok(true);
                    }
                    if stop.load(Ordering::Relaxed) {
                        return Ok(false);
                    }
                    if descriptors
                        .first()
                        .and_then(PollFd::revents)
                        .is_some_and(|flags| {
                            flags.intersects(
                                PollFlags::POLLIN | PollFlags::POLLHUP | PollFlags::POLLERR,
                            )
                        })
                    {
                        return Ok(true);
                    }
                }
                Err(nix::errno::Errno::EINTR) => {}
                Err(error) => return Err(io::Error::other(error)),
            }
        }
    }
    #[cfg(not(unix))]
    {
        Ok(!stop.load(Ordering::Relaxed))
    }
}

struct BoundedLine {
    bytes: Vec<u8>,
    truncated: bool,
    structured: bool,
}

#[derive(Default)]
struct LineAccumulator {
    bytes: Vec<u8>,
    structured: Option<bool>,
    truncated: bool,
}

impl LineAccumulator {
    fn push(&mut self, chunk: &[u8]) -> Vec<BoundedLine> {
        let mut lines = Vec::new();
        for byte in chunk {
            self.push_byte(*byte);
            if *byte == b'\n' {
                lines.push(self.take_line());
            }
        }
        lines
    }

    fn finish(&mut self) -> Option<BoundedLine> {
        if self.bytes.is_empty() {
            None
        } else {
            Some(self.take_line())
        }
    }

    fn push_byte(&mut self, byte: u8) {
        if self.structured.is_none() && !byte.is_ascii_whitespace() {
            self.structured = Some(matches!(byte, b'{' | b'['));
        }
        if self.structured == Some(false) && self.bytes.len() > MAX_LINE_BYTES {
            self.bytes.truncate(MAX_LINE_BYTES);
            self.truncated = true;
        }
        let limit = if self.structured == Some(false) {
            MAX_LINE_BYTES
        } else {
            MAX_STRUCTURED_LINE_BYTES
        };
        if self.bytes.len() < limit {
            self.bytes.push(byte);
        } else {
            self.truncated = true;
        }
    }

    fn take_line(&mut self) -> BoundedLine {
        let line = BoundedLine {
            bytes: std::mem::take(&mut self.bytes),
            truncated: self.truncated,
            structured: self.structured == Some(true),
        };
        self.structured = None;
        self.truncated = false;
        line
    }
}

fn read_chunk<R: Read + AsFd>(
    reader: &mut BufReader<R>,
    stop: &AtomicBool,
) -> io::Result<Option<Vec<u8>>> {
    if !wait_until_readable(reader, stop)? {
        return Ok(None);
    }
    let available = reader.fill_buf()?;
    if available.is_empty() {
        return Ok(None);
    }
    let chunk = available.to_vec();
    reader.consume(chunk.len());
    Ok(Some(chunk))
}

fn trim_line_end(mut text: String) -> String {
    if text.ends_with('\r') {
        text.pop();
    }
    text
}

#[derive(Default)]
struct StreamCompletion {
    stdout: bool,
    stderr: bool,
}

#[derive(Default)]
pub(super) struct CaptureState {
    digest: Digest,
    streams: StreamCompletion,
    channel_closed: bool,
    wrapper_failure: bool,
    log_failure: bool,
}

impl CaptureState {
    pub(super) fn drain(&mut self, receiver: &Receiver<ReaderEvent>, sinks: &mut CaptureSinks<'_>) {
        // Keep continuous child output from starving deadline checks.
        for _ in 0..EVENT_CHANNEL_CAPACITY {
            let Ok(event) = receiver.try_recv() else {
                break;
            };
            self.handle(event, sinks);
        }
    }

    pub(super) fn handle(&mut self, event: ReaderEvent, sinks: &mut CaptureSinks<'_>) {
        match event {
            ReaderEvent::Raw(stream, bytes) => {
                if !self.log_failure
                    && let Some(sink) = sinks.log.as_mut()
                    && sink.write_chunk(&bytes).is_err()
                {
                    self.log_failure = true;
                }
                if let Some(sink) = sinks.output.as_mut()
                    && sink.write_chunk(stream, &bytes).is_err()
                {
                    self.wrapper_failure = true;
                }
            }
            ReaderEvent::Line(line) => {
                self.digest.ingest(&line);
                if let Some(sink) = sinks.line.as_mut()
                    && sink.write_line(&line).is_err()
                {
                    self.wrapper_failure = true;
                }
            }
            ReaderEvent::Finished(stream) => {
                if let Some(sink) = sinks.output.as_mut()
                    && sink.finish_stream(stream).is_err()
                {
                    self.wrapper_failure = true;
                }
                match stream {
                    Stream::Stdout => self.streams.stdout = true,
                    Stream::Stderr => self.streams.stderr = true,
                }
            }
            ReaderEvent::Failed(_stream) => self.wrapper_failure = true,
        }
    }
    pub(super) const fn streams_done(&self) -> bool {
        self.streams.stdout && self.streams.stderr
    }
    pub(super) const fn channel_closed(&self) -> bool {
        self.channel_closed
    }

    pub(super) const fn close_channel(&mut self) {
        self.channel_closed = true;
        if !self.streams_done() {
            self.wrapper_failure = true;
        }
    }

    pub(super) const fn is_finished(&self) -> bool {
        self.streams_done() || self.channel_closed
    }

    pub(super) const fn wrapper_failure(&self) -> bool {
        self.wrapper_failure
    }

    pub(super) const fn log_failure(&self) -> bool {
        self.log_failure
    }

    pub(super) const fn mark_wrapper_failure(&mut self) {
        self.wrapper_failure = true;
    }

    pub(super) fn take_digest(&mut self) -> Digest {
        std::mem::take(&mut self.digest)
    }
}
