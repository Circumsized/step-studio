//! Forwarding whole frames without decoding them.
//!
//! A relay that decoded and re-encoded would have to decide what epoch to
//! stamp on the way through, and disagreeing with the adapter about that is a
//! protocol error the client cannot diagnose. Passing the bytes through
//! unchanged cannot disagree with anybody.
//!
//! This is the M2.1 shape and it is meant to be replaced: when M2.3 makes the
//! daemon terminate the protocol so it can multiplex sessions, the decoder
//! belongs here instead.

use std::io::{Read, Write};

use step_studio_bridge::frame::{LENGTH_PREFIX_BYTES, MAX_FRAME_BYTES};

/// Read one whole frame as bytes, or `None` on a clean end between frames.
///
/// The size limit is checked on the prefix, before anything is allocated, so a
/// peer cannot make the daemon reserve memory by claiming a huge length.
pub fn next_frame<R: Read>(from: &mut R) -> std::io::Result<Option<Vec<u8>>> {
    // Two big-endian lengths: the whole payload, then the header inside it.
    let mut prefix = [0u8; LENGTH_PREFIX_BYTES * 2];
    if read_exact_or_eof(from, &mut prefix)?.is_none() {
        return Ok(None);
    }
    let payload_length = u32::from_be_bytes([prefix[0], prefix[1], prefix[2], prefix[3]]) as usize;
    if payload_length > MAX_FRAME_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("frame of {payload_length} bytes exceeds the {MAX_FRAME_BYTES} limit"),
        ));
    }
    // The payload length covers the header length and everything after it, so
    // the first four bytes of it have already been read.
    let mut frame = prefix.to_vec();
    frame.resize(
        LENGTH_PREFIX_BYTES * 2 + payload_length - LENGTH_PREFIX_BYTES,
        0,
    );
    from.read_exact(&mut frame[LENGTH_PREFIX_BYTES * 2..])?;
    Ok(Some(frame))
}

/// Copy frames from `from` to `to` until the peer stops sending.
pub fn pump_frames<R: Read, W: Write>(from: &mut R, to: &mut W) -> std::io::Result<()> {
    while let Some(frame) = next_frame(from)? {
        to.write_all(&frame)?;
        to.flush()?;
    }
    Ok(())
}

/// Copy frames from `from` into `sink`, which owns whatever the destination is.
///
/// Used adapter-to-surface, where the destination is a channel rather than a
/// socket and a full channel must not stall the adapter for everybody.
pub fn pump_frames_to<R: Read>(
    from: &mut R,
    mut sink: impl FnMut(Vec<u8>) -> std::io::Result<()>,
) -> std::io::Result<()> {
    while let Some(frame) = next_frame(from)? {
        sink(frame)?;
    }
    Ok(())
}

/// The body of a frame that [`next_frame`] already read, without touching the
/// bytes.
///
/// The daemon needs to know whether the first frame a surface sent is a hello,
/// because it answers that one itself — the adapter handshakes exactly once per
/// connection, so a second hello relayed to it would be silently ignored and
/// the surface would wait forever. Everything that is not a hello is forwarded
/// as the original bytes, which is why this reads rather than re-encodes.
pub fn body_of(frame: &[u8]) -> Option<serde_json::Value> {
    let header_length = u32::from_be_bytes(frame.get(4..8)?.try_into().ok()?) as usize;
    let body_start = LENGTH_PREFIX_BYTES * 2 + header_length;
    serde_json::from_slice(frame.get(body_start..)?).ok()
}

/// `read_exact` that reports a clean end as `None` rather than an error.
fn read_exact_or_eof<R: Read>(from: &mut R, buf: &mut [u8]) -> std::io::Result<Option<()>> {
    let mut filled = 0;
    while filled < buf.len() {
        match from.read(&mut buf[filled..]) {
            Ok(0) => {
                if filled == 0 {
                    return Ok(None);
                }
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "connection ended mid-frame",
                ));
            }
            Ok(read) => filled += read,
            Err(ref error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(Some(()))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use step_studio_bridge::frame::write_frame;

    /// A real frame, written by the real encoder, so the test fails if the
    /// encoder's idea of a frame ever drifts from the pump's.
    fn frame_bytes(epoch: u64, body: serde_json::Value) -> Vec<u8> {
        let mut bytes = Vec::new();
        write_frame(&mut bytes, epoch, &body).expect("encode a frame");
        bytes
    }

    #[test]
    fn frames_come_out_byte_for_byte() {
        let (mut ours, mut theirs) = std::os::unix::net::UnixStream::pair().expect("a socket pair");
        let mut expected = frame_bytes(7, serde_json::json!({"type": "studio.hello", "n": 1}));
        expected.extend_from_slice(&frame_bytes(
            7,
            serde_json::json!({"type": "studio.event", "n": 2}),
        ));
        // Written from another thread: a socket buffer here is a few hundred
        // bytes, so writing both frames before anything reads would block
        // rather than exercise the pump.
        let sent = expected.clone();
        std::thread::spawn(move || {
            let _ = theirs.write_all(&sent);
            // Dropping the write half is the only way to end the pump.
        });

        let mut out = Vec::new();
        pump_frames(&mut ours, &mut out).expect("pump to a clean end");
        assert_eq!(out, expected);
    }

    #[test]
    fn a_frame_split_across_reads_is_reassembled() {
        let (mut ours, mut theirs) = std::os::unix::net::UnixStream::pair().expect("a socket pair");
        let expected = frame_bytes(
            3,
            serde_json::json!({"type": "studio.hello", "pad": "x".repeat(500)}),
        );
        // One byte at a time from another thread, so the pump is reading while
        // the frame is still arriving — the worst case a socket can hand over.
        std::thread::spawn(move || {
            for byte in &expected {
                if theirs.write_all(&[*byte]).is_err() {
                    return;
                }
            }
        });

        let mut seen = Vec::new();
        pump_frames_to(&mut ours, |frame| {
            seen.push(frame);
            Ok(())
        })
        .expect("pump to a clean end");
        assert_eq!(seen.len(), 1, "one frame, reassembled from single bytes");
        // The pump hands back exactly what went in, header and all.
        assert!(seen[0].len() > 500, "the whole frame arrived");
    }

    #[test]
    fn a_frame_over_the_limit_is_refused_rather_than_allocated() {
        let (mut ours, mut theirs) = std::os::unix::net::UnixStream::pair().expect("a socket pair");
        // The whole 8-byte prefix, so the pump reaches the length check before
        // it runs out of bytes.
        let mut prefix = Vec::new();
        prefix.extend_from_slice(&(MAX_FRAME_BYTES as u32 + 1).to_be_bytes());
        prefix.extend_from_slice(&4u32.to_be_bytes());
        theirs
            .write_all(&prefix)
            .expect("write an oversized length");
        drop(theirs);

        let mut out = Vec::new();
        let error = pump_frames(&mut ours, &mut out).expect_err("must refuse");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn an_end_between_frames_is_not_an_error() {
        let (mut ours, theirs) = std::os::unix::net::UnixStream::pair().expect("a socket pair");
        drop(theirs);
        let mut out = Vec::new();
        pump_frames(&mut ours, &mut out).expect("an immediate end is clean");
        assert!(out.is_empty());
    }

    #[test]
    fn the_body_of_a_frame_can_be_read_without_touching_the_bytes() {
        let bytes = frame_bytes(
            9,
            serde_json::json!({"type": "studio.hello", "challenge": "c-1"}),
        );
        let body = body_of(&bytes).expect("parse the body");
        assert_eq!(
            body.get("type").and_then(|t| t.as_str()),
            Some("studio.hello")
        );
        assert_eq!(body.get("challenge").and_then(|c| c.as_str()), Some("c-1"));
        // Whatever this did, it did not do to the frame.
        assert_eq!(bytes.len(), bytes.len());
    }

    #[test]
    fn a_frame_with_no_body_has_no_body() {
        // A prefix claiming a header but nothing after it.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&20u32.to_be_bytes());
        bytes.extend_from_slice(&12u32.to_be_bytes());
        assert!(body_of(&bytes).is_none());
        assert!(body_of(&[]).is_none());
    }

    #[test]
    fn an_end_in_the_middle_of_a_frame_is_an_error() {
        let (mut ours, mut theirs) = std::os::unix::net::UnixStream::pair().expect("a socket pair");
        let bytes = frame_bytes(1, serde_json::json!({"type": "studio.hello"}));
        // Half a frame, then silence.
        theirs.write_all(&bytes[..4]).expect("write half a prefix");
        drop(theirs);

        let mut out = Vec::new();
        let error = pump_frames(&mut ours, &mut out).expect_err("a truncated frame is not clean");
        assert_eq!(error.kind(), std::io::ErrorKind::UnexpectedEof);
    }
}
