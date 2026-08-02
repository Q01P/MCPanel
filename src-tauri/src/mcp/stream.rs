//! stdout/stderr pipelines: one async task per stream, `LinesCodec` framing
//! with a hard single-line cap, ANSI/control-byte stripping, and bounded
//! channels that drop lines under pressure — the panel must never choke on a
//! flooding server.

use std::borrow::Cow;
use std::sync::Arc;

use bytes::{Buf, BytesMut};
use tokio::io::AsyncRead;
use tokio::sync::mpsc;
use tokio_stream::StreamExt;
use tokio_util::codec::{Decoder, FramedRead};
use tracing::debug;

use crate::error::AppResult;

/// Single-line cap: one endless line must not balloon memory; the remainder
/// up to the next newline is discarded.
pub const MAX_LINE_LENGTH: usize = 64 * 1024;

/// Bounded capacity per stream channel.
pub const CHANNEL_CAPACITY: usize = 1024;

/// What a stream pump delivers. Lines are `Arc<str>` so fan-out across
/// threads never clones the payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StreamEvent {
    Line(Arc<str>),
    /// Lines lost since the last delivered event — backpressure drops and
    /// over-length discards combined. The UI renders this as a
    /// dropped-lines marker (T11).
    Dropped(u64),
}

/// The per-child stream pair; pumps run until EOF (child exit) or until the
/// receivers are dropped.
pub struct ChildStreams {
    pub stdout: mpsc::Receiver<StreamEvent>,
    pub stderr: mpsc::Receiver<StreamEvent>,
}

/// Take the piped stdout/stderr off a child and start the two pump tasks.
pub fn attach(child: &mut tokio::process::Child) -> AppResult<ChildStreams> {
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| std::io::Error::other("child stdout not piped"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| std::io::Error::other("child stderr not piped"))?;

    let (stdout_tx, stdout_rx) = mpsc::channel(CHANNEL_CAPACITY);
    let (stderr_tx, stderr_rx) = mpsc::channel(CHANNEL_CAPACITY);
    tokio::spawn(pump(stdout, stdout_tx));
    tokio::spawn(pump(stderr, stderr_tx));

    Ok(ChildStreams {
        stdout: stdout_rx,
        stderr: stderr_rx,
    })
}

/// Read frames, sanitize, and forward without ever awaiting channel space:
/// a full channel means the line is dropped and counted, not that the pump
/// (and eventually the child's pipe) stalls.
async fn pump<R: AsyncRead + Unpin + Send + 'static>(
    reader: R,
    tx: mpsc::Sender<StreamEvent>,
) {
    let mut frames = FramedRead::new(reader, CappedLines::default());
    let mut forwarder = BoundedForwarder::new(tx);

    while let Some(item) = frames.next().await {
        match item {
            Ok(RawLine::Line(raw)) => {
                let line: Arc<str> = Arc::from(strip_ansi(&raw).as_ref());
                if !forwarder.offer(line) {
                    return; // receiver gone
                }
            }
            Ok(RawLine::Oversized) => {
                debug!(target: "app::stream", "line exceeded {MAX_LINE_LENGTH} bytes, discarded");
                forwarder.lose(1);
            }
            Err(_) => break, // pipe error — stream is gone
        }
    }
    forwarder.finish().await;
}

/// Non-blocking delivery with faithful drop accounting: a pending gap marker
/// is flushed before the next line so ordering in the UI is
/// [lines] [gap marker] [lines]; anything lost is counted, never silently
/// vanished. Shared by the stream pumps and the protocol router (T5).
pub(crate) struct BoundedForwarder {
    tx: mpsc::Sender<StreamEvent>,
    lost: u64,
}

impl BoundedForwarder {
    pub(crate) fn new(tx: mpsc::Sender<StreamEvent>) -> Self {
        Self { tx, lost: 0 }
    }

    pub(crate) fn lose(&mut self, n: u64) {
        self.lost += n;
    }

    /// Deliver a line without awaiting channel space; returns false when the
    /// receiver is gone.
    pub(crate) fn offer(&mut self, line: Arc<str>) -> bool {
        if self.lost > 0 {
            match self.tx.try_send(StreamEvent::Dropped(self.lost)) {
                Ok(()) => self.lost = 0,
                Err(mpsc::error::TrySendError::Full(_)) => {
                    self.lost += 1; // no room for the marker → this line joins the gap
                    return true;
                }
                Err(mpsc::error::TrySendError::Closed(_)) => return false,
            }
        }
        match self.tx.try_send(StreamEvent::Line(line)) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.lost += 1;
                true
            }
            Err(mpsc::error::TrySendError::Closed(_)) => false,
        }
    }

    /// EOF: the producer is done, so a blocking send is fine — the final
    /// marker arrives as soon as the receiver drains.
    pub(crate) async fn finish(self) {
        if self.lost > 0 {
            let _ = self.tx.send(StreamEvent::Dropped(self.lost)).await;
        }
    }
}

enum RawLine {
    Line(String),
    /// A line that blew past [`MAX_LINE_LENGTH`]; its bytes were discarded.
    Oversized,
}

/// Newline framing with a hard length cap. Unlike `LinesCodec`, an oversized
/// line is an *item*, not a decode error — `FramedRead` permanently
/// terminates a stream after any codec error (`has_errored`, tokio-util
/// 0.7), which would let one hostile line kill the whole log pipeline.
/// Invalid UTF-8 is replaced lossily; these are logs, not the protocol layer.
#[derive(Default)]
struct CappedLines {
    discarding: bool,
}

impl CappedLines {
    fn take_line(buf: &mut BytesMut, newline_at: usize) -> RawLine {
        let mut line = buf.split_to(newline_at + 1);
        line.truncate(line.len() - 1); // the \n
        if line.last() == Some(&b'\r') {
            line.truncate(line.len() - 1);
        }
        if line.len() > MAX_LINE_LENGTH {
            return RawLine::Oversized;
        }
        RawLine::Line(String::from_utf8_lossy(&line).into_owned())
    }
}

impl Decoder for CappedLines {
    type Item = RawLine;
    type Error = std::io::Error;

    fn decode(&mut self, buf: &mut BytesMut) -> Result<Option<RawLine>, Self::Error> {
        loop {
            let newline = buf.iter().position(|b| *b == b'\n');
            if self.discarding {
                match newline {
                    // Tail of the oversized line (already reported) — skip it.
                    Some(at) => {
                        buf.advance(at + 1);
                        self.discarding = false;
                        continue;
                    }
                    None => {
                        buf.clear();
                        return Ok(None);
                    }
                }
            }
            return match newline {
                Some(at) => Ok(Some(Self::take_line(buf, at))),
                None if buf.len() > MAX_LINE_LENGTH => {
                    buf.clear();
                    self.discarding = true;
                    Ok(Some(RawLine::Oversized))
                }
                None => Ok(None),
            };
        }
    }

    fn decode_eof(&mut self, buf: &mut BytesMut) -> Result<Option<RawLine>, Self::Error> {
        if let Some(frame) = self.decode(buf)? {
            return Ok(Some(frame));
        }
        if self.discarding || buf.is_empty() {
            return Ok(None);
        }
        // Trailing line without a newline.
        let last = buf.split_to(buf.len());
        Ok(Some(RawLine::Line(
            String::from_utf8_lossy(&last).into_owned(),
        )))
    }
}

/// Strip ANSI escape sequences (CSI, OSC, two-char escapes) and stray control
/// bytes (tab survives). Borrows when the line is already clean.
pub fn strip_ansi(raw: &str) -> Cow<'_, str> {
    if !raw.chars().any(|c| c.is_control()) {
        return Cow::Borrowed(raw);
    }

    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => match chars.next() {
                // CSI: parameters until a final byte in 0x40..=0x7e.
                Some('[') => {
                    for follow in chars.by_ref() {
                        if matches!(follow, '\x40'..='\x7e') {
                            break;
                        }
                    }
                }
                // OSC: terminated by BEL or ESC \ (string terminator).
                Some(']') => {
                    while let Some(follow) = chars.next() {
                        if follow == '\x07' {
                            break;
                        }
                        if follow == '\x1b' {
                            chars.next();
                            break;
                        }
                    }
                }
                // Two-char escape — both consumed.
                _ => {}
            },
            '\t' => out.push('\t'),
            c if c.is_control() => {} // stray garbage byte — drop
            c => out.push(c),
        }
    }
    Cow::Owned(out)
}

/// Salvage helper for NDJSON routing (T5): the payload from the first `{`,
/// for lines where a misbehaving server prefixed garbage.
pub fn json_candidate(line: &str) -> Option<&str> {
    line.find('{').map(|start| &line[start..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_csi_colors_and_stray_controls() {
        assert_eq!(
            strip_ansi("\x1b[1;31merror:\x1b[0m fine \x1b[4munderlined\x1b[0m"),
            "error: fine underlined"
        );
        assert_eq!(strip_ansi("\u{1}\u{2}\u{7f} pre-JSON garbage"), " pre-JSON garbage");
        assert_eq!(strip_ansi("\x1b]0;window title\x07visible"), "visible");
        assert_eq!(strip_ansi("keep\tthe tab"), "keep\tthe tab");
        assert!(matches!(strip_ansi("already clean"), Cow::Borrowed(_)));
    }

    #[test]
    fn json_candidate_trims_garbage_prefixes() {
        assert_eq!(
            json_candidate("junk>>{\"jsonrpc\":\"2.0\"}"),
            Some("{\"jsonrpc\":\"2.0\"}")
        );
        assert_eq!(json_candidate("no json here"), None);
    }

    async fn pump_into(input: Vec<u8>, capacity: usize) -> mpsc::Receiver<StreamEvent> {
        let (tx, rx) = mpsc::channel(capacity);
        tokio::spawn(pump(std::io::Cursor::new(input), tx));
        rx
    }

    #[tokio::test]
    async fn full_channel_drops_lines_and_reports_the_gap() {
        let input: Vec<u8> = (0..100).flat_map(|n| format!("line {n}\n").into_bytes()).collect();
        let mut rx = pump_into(input, 4).await;

        let mut lines = 0u64;
        let mut dropped = 0u64;
        while let Some(event) = rx.recv().await {
            match event {
                StreamEvent::Line(_) => lines += 1,
                StreamEvent::Dropped(n) => dropped += n,
            }
        }
        assert_eq!(lines + dropped, 100, "no line may vanish unaccounted");
        assert!(lines <= 5, "channel bound was not respected: {lines}");
        assert!(dropped >= 95);
    }

    #[tokio::test]
    async fn oversized_lines_are_discarded_and_counted() {
        let mut input = b"before\n".to_vec();
        input.extend(std::iter::repeat_n(b'x', MAX_LINE_LENGTH + 100));
        input.extend(b"\nafter\n");
        let mut rx = pump_into(input, 16).await;

        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            events.push(event);
        }
        assert_eq!(
            events,
            vec![
                StreamEvent::Line(Arc::from("before")),
                StreamEvent::Dropped(1),
                StreamEvent::Line(Arc::from("after")),
            ]
        );
    }
}
