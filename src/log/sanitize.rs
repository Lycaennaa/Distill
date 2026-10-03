use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::time::Instant;

use sha2::{Digest as ShaDigest, Sha256};

use crate::process::CancellationToken;
use crate::redaction::{RedactionPolicy, redact};
use crate::status::StatusClass;

const COPY_BUFFER_BYTES: usize = 8_192;
pub(super) const MAX_PENDING_LINE_BYTES: usize = 4_194_304;

pub(super) enum PublishFailure {
    Stopped(StatusClass),
    Io(String),
}

pub(super) fn sanitize_file(
    input_file: File,
    output_file: File,
    policy: RedactionPolicy,
    deadline: Option<Instant>,
    cancellation: &CancellationToken,
) -> Result<(u64, [u8; 32]), PublishFailure> {
    let mut input = input_file;
    let mut output = BufWriter::new(output_file);
    let mut hasher = Sha256::new();
    let mut bytes_written = 0_u64;
    let mut pending = Vec::new();
    let mut buffer = [0_u8; COPY_BUFFER_BYTES];

    loop {
        if let Some(class) = super::stop_class(deadline, cancellation) {
            return Err(PublishFailure::Stopped(class));
        }
        let count = input
            .read(&mut buffer)
            .map_err(|error| PublishFailure::Io(error.to_string()))?;
        if count == 0 {
            break;
        }
        let Some(chunk) = buffer.get(..count) else {
            return Err(PublishFailure::Io("invalid log read size".to_owned()));
        };
        pending.extend_from_slice(chunk);
        while let Some(newline) = pending.iter().position(|byte| *byte == b'\n') {
            let end = newline.saturating_add(1);
            if end > MAX_PENDING_LINE_BYTES {
                return Err(PublishFailure::Io(
                    "log line exceeds sanitizer limit".to_owned(),
                ));
            }
            let line: Vec<u8> = pending.drain(..end).collect();
            write_sanitized(&line, policy, &mut output, &mut hasher, &mut bytes_written)?;
        }
        if pending.len() > MAX_PENDING_LINE_BYTES {
            return Err(PublishFailure::Io(
                "log line exceeds sanitizer limit".to_owned(),
            ));
        }
    }
    if !pending.is_empty() {
        write_sanitized(
            &pending,
            policy,
            &mut output,
            &mut hasher,
            &mut bytes_written,
        )?;
    }
    output
        .flush()
        .map_err(|error| PublishFailure::Io(error.to_string()))?;
    output
        .get_ref()
        .sync_all()
        .map_err(|error| PublishFailure::Io(error.to_string()))?;
    let digest = hasher.finalize();
    let mut checksum = [0_u8; 32];
    checksum.copy_from_slice(&digest);
    Ok((bytes_written, checksum))
}

fn write_sanitized(
    bytes: &[u8],
    policy: RedactionPolicy,
    output: &mut BufWriter<File>,
    hasher: &mut Sha256,
    bytes_written: &mut u64,
) -> Result<(), PublishFailure> {
    let text = String::from_utf8_lossy(bytes);
    let sanitized = redact(text.as_ref(), policy);
    let length = u64::try_from(sanitized.len())
        .map_err(|_| PublishFailure::Io("sanitized log is too large".to_owned()))?;
    *bytes_written = bytes_written
        .checked_add(length)
        .ok_or_else(|| PublishFailure::Io("sanitized log is too large".to_owned()))?;
    output
        .write_all(sanitized.as_bytes())
        .map_err(|error| PublishFailure::Io(error.to_string()))?;
    hasher.update(sanitized.as_bytes());
    Ok(())
}

pub(super) fn checksum_text(checksum: [u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(64);
    for byte in checksum {
        for nibble in [byte >> 4, byte & 0x0f] {
            let value = HEX.get(usize::from(nibble)).copied().unwrap_or(b'?');
            result.push(char::from(value));
        }
    }
    result
}
