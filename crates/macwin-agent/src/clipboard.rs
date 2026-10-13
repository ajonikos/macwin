//! The clipboard thread of a session: it owns the clipboard backend, polls
//! its change count, reports local changes and writes the peer's content.
//!
//! All backend calls happen on this one thread: the Windows clipboard may
//! block for a moment when another process holds it, and AppKit objects are
//! best kept off the async runtime. Copied files are walked here too (their
//! manifest is built before they are reported), and images are decoded
//! here (see [`LastImage`]). Changes caused by the worker's own writes are
//! not reported (it remembers the change count after writing).

use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::PathBuf;
use std::sync::mpsc as std_mpsc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use macwin_core::clipboard::LocalClipboard;
use macwin_platform::image::decode_png;
use macwin_platform::{ClipboardBackend, ClipboardContent, PlatformError, SnapshotLimits};
use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::transfer::{ManifestError, build_manifest};

/// How often the change count is checked.
pub const POLL_INTERVAL: Duration = Duration::from_millis(250);
/// Local changes queued towards the driver.
const EVENT_QUEUE: usize = 16;

/// Content to put on the local clipboard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardWrite {
    Text(String),
    /// PNG.
    Image(Vec<u8>),
    /// Existing files and folders (received into the staging directory).
    Files(Vec<PathBuf>),
}

enum Command {
    Write(ClipboardWrite),
    Stop,
}

/// What the worker reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkerLimits {
    pub snapshot: SnapshotLimits,
    /// Copied files above this total are reported as too large.
    pub max_files_bytes: u64,
}

/// Handle of a running clipboard thread; stops it when dropped.
pub struct ClipboardWorker {
    commands: std_mpsc::Sender<Command>,
    join: Option<JoinHandle<()>>,
}

impl ClipboardWorker {
    /// Starts the thread. Changes made after this call (except the worker's
    /// own writes) arrive on the returned receiver; the current content is
    /// not reported.
    pub fn start(
        backend: Box<dyn ClipboardBackend>,
        limits: WorkerLimits,
        poll: Duration,
    ) -> Result<(ClipboardWorker, mpsc::Receiver<LocalClipboard>), PlatformError> {
        let baseline = backend.change_count()?;
        let (commands, command_rx) = std_mpsc::channel();
        let (events_tx, events) = mpsc::channel(EVENT_QUEUE);
        let join = thread::Builder::new()
            .name("macwin-clipboard".into())
            .spawn(move || run(backend, baseline, limits, poll, command_rx, events_tx))
            .map_err(|e| {
                PlatformError::Backend(format!("cannot start the clipboard thread: {e}"))
            })?;
        Ok((
            ClipboardWorker {
                commands,
                join: Some(join),
            },
            events,
        ))
    }

    /// Replaces the local clipboard (asynchronously).
    pub fn write(&self, content: ClipboardWrite) {
        let _ = self.commands.send(Command::Write(content));
    }

    /// Stops the thread and waits for it (at most one poll interval, plus a
    /// file walk in progress).
    pub fn stop(&mut self) {
        let _ = self.commands.send(Command::Stop);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl Drop for ClipboardWorker {
    fn drop(&mut self) {
        self.stop();
    }
}

/// A hash of the picture a PNG holds; `None` when it cannot be decoded.
fn pixel_signature(png: &[u8]) -> Option<u64> {
    let img = decode_png(png).ok()?;
    let mut h = DefaultHasher::new();
    (img.width, img.height).hash(&mut h);
    img.pixels.hash(&mut h);
    Some(h.finish())
}

/// The last image the worker read or wrote. The OS or a clipboard manager
/// may re-encode a picture (a TIFF on the Mac, a bitmap on Windows, another
/// PNG): such a copy is reported with the bytes seen before, so the sync
/// logic, which compares bytes, recognises it as the same content and a
/// re-copied image never bounces between the machines.
#[derive(Default)]
struct LastImage(Option<(u64, Vec<u8>)>);

impl LastImage {
    fn remember(&mut self, png: &[u8]) {
        self.0 = pixel_signature(png).map(|sig| (sig, png.to_vec()));
    }

    /// `png`, or the earlier bytes of the same picture.
    fn normalize(&mut self, png: Vec<u8>) -> Vec<u8> {
        let Some(sig) = pixel_signature(&png) else {
            return png;
        };
        match &self.0 {
            Some((last, earlier)) if *last == sig => earlier.clone(),
            _ => {
                self.0 = Some((sig, png.clone()));
                png
            }
        }
    }
}

/// What the sync logic sees for a snapshot: copied files become a manifest.
fn to_local(
    content: ClipboardContent,
    max_files_bytes: u64,
    last_image: &mut LastImage,
) -> LocalClipboard {
    match content {
        ClipboardContent::Ours => LocalClipboard::Ours,
        ClipboardContent::Sensitive => LocalClipboard::Sensitive,
        ClipboardContent::Unsupported => LocalClipboard::Unsupported,
        ClipboardContent::TooLarge { bytes } => LocalClipboard::TooLarge { bytes },
        ClipboardContent::Text(t) => LocalClipboard::Text(t),
        ClipboardContent::Image(png) => LocalClipboard::Image(last_image.normalize(png)),
        ClipboardContent::Files(paths) => match build_manifest(&paths, max_files_bytes) {
            Ok(files) => LocalClipboard::Files(files),
            Err(ManifestError::TooLarge { bytes }) => LocalClipboard::TooLarge { bytes },
            Err(ManifestError::Empty) => LocalClipboard::Unsupported,
            Err(e) => {
                warn!(error = %e, "copied files are not shared");
                LocalClipboard::Unsupported
            }
        },
    }
}

fn write(backend: &dyn ClipboardBackend, content: &ClipboardWrite) -> Result<u64, PlatformError> {
    match content {
        ClipboardWrite::Text(t) => backend.write_text(t),
        ClipboardWrite::Image(png) => backend.write_image(png),
        ClipboardWrite::Files(paths) => backend.write_files(paths),
    }
}

/// `first`, or the newest command queued behind it: only the last of
/// several writes matters, and a stop wins over writes.
fn newest(
    commands: &std_mpsc::Receiver<Command>,
    first: Result<Command, std_mpsc::RecvTimeoutError>,
) -> Result<Command, std_mpsc::RecvTimeoutError> {
    let mut cmd = first?;
    while !matches!(cmd, Command::Stop) {
        match commands.try_recv() {
            Ok(next) => cmd = next,
            Err(_) => break,
        }
    }
    Ok(cmd)
}

fn run(
    backend: Box<dyn ClipboardBackend>,
    baseline: u64,
    limits: WorkerLimits,
    poll: Duration,
    commands: std_mpsc::Receiver<Command>,
    events: mpsc::Sender<LocalClipboard>,
) {
    let mut seen = baseline;
    let mut last_image = LastImage::default();
    loop {
        match newest(&commands, commands.recv_timeout(poll)) {
            Ok(Command::Write(content)) => match write(backend.as_ref(), &content) {
                Ok(count) => {
                    seen = count;
                    if let ClipboardWrite::Image(png) = &content {
                        last_image.remember(png);
                    }
                }
                Err(e) => warn!(error = %e, "cannot write the clipboard"),
            },
            Ok(Command::Stop) | Err(std_mpsc::RecvTimeoutError::Disconnected) => return,
            Err(std_mpsc::RecvTimeoutError::Timeout) => {}
        }
        let count = match backend.change_count() {
            Ok(c) => c,
            Err(e) => {
                debug!(error = %e, "cannot read the clipboard change count");
                continue;
            }
        };
        if count == seen {
            continue;
        }
        match backend.snapshot(&limits.snapshot) {
            Ok(snap) => {
                seen = snap.change_count;
                let local = to_local(snap.content, limits.max_files_bytes, &mut last_image);
                if events.blocking_send(local).is_err() {
                    return;
                }
            }
            Err(e) => {
                seen = count;
                warn!(error = %e, "cannot read the clipboard");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use macwin_platform::testing::{FakeClipboard, FakeWrite};

    use super::*;

    const POLL: Duration = Duration::from_millis(5);

    fn limits() -> WorkerLimits {
        WorkerLimits {
            snapshot: SnapshotLimits {
                max_text_bytes: 100,
                images: true,
                max_image_bytes: 100,
                files: true,
            },
            max_files_bytes: 1000,
        }
    }

    async fn next(rx: &mut mpsc::Receiver<LocalClipboard>) -> LocalClipboard {
        tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("no clipboard event")
            .expect("worker gone")
    }

    #[tokio::test]
    async fn reports_changes_but_not_its_own_writes() {
        let (cb, user) = FakeClipboard::new();
        user.copy_text("before start");
        let (mut worker, mut rx) = ClipboardWorker::start(Box::new(cb), limits(), POLL).unwrap();
        user.copy_text("hello");
        assert_eq!(next(&mut rx).await, LocalClipboard::Text("hello".into()));
        worker.write(ClipboardWrite::Text("from peer".into()));
        let deadline = Instant::now() + Duration::from_secs(5);
        while user.writes().is_empty() {
            assert!(Instant::now() < deadline, "write not executed");
            tokio::time::sleep(POLL).await;
        }
        assert_eq!(user.text().as_deref(), Some("from peer"));
        user.copy_image(&[1, 2, 3]);
        // The own write produced no event: the next one is the image.
        assert_eq!(next(&mut rx).await, LocalClipboard::Image(vec![1, 2, 3]));
        user.copy_sensitive("pw");
        assert_eq!(next(&mut rx).await, LocalClipboard::Sensitive);
        worker.write(ClipboardWrite::Image(vec![9]));
        while user.writes().len() < 2 {
            assert!(Instant::now() < deadline, "writes not executed");
            tokio::time::sleep(POLL).await;
        }
        worker.write(ClipboardWrite::Files(vec![PathBuf::from("/staging/a")]));
        while user.writes().len() < 3 {
            assert!(Instant::now() < deadline, "writes not executed");
            tokio::time::sleep(POLL).await;
        }
        assert_eq!(
            user.writes()[1..],
            [
                FakeWrite::Image(vec![9]),
                FakeWrite::Files(vec![PathBuf::from("/staging/a")])
            ]
        );
        worker.stop();
        user.copy_text("after stop");
        tokio::time::sleep(POLL * 4).await;
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn only_the_newest_queued_write_is_applied_and_stop_wins() {
        let (tx, rx) = std_mpsc::channel();
        let text = |t: &str| Command::Write(ClipboardWrite::Text(t.into()));
        let written = |cmd: Result<Command, _>| match cmd {
            Ok(Command::Write(ClipboardWrite::Text(t))) => Some(t),
            _ => None,
        };
        for t in ["a", "b", "c"] {
            tx.send(text(t)).unwrap();
        }
        let first = rx.recv_timeout(POLL);
        assert_eq!(written(newest(&rx, first)).as_deref(), Some("c"));
        tx.send(text("d")).unwrap();
        tx.send(Command::Stop).unwrap();
        tx.send(text("e")).unwrap();
        let first = rx.recv_timeout(POLL);
        assert!(matches!(newest(&rx, first), Ok(Command::Stop)));
        assert!(
            newest(&rx, rx.recv_timeout(POLL)).is_ok(),
            "e is still queued"
        );
        assert!(newest(&rx, rx.recv_timeout(POLL)).is_err());
    }

    /// An opaque picture as an RGBA PNG (what macwin writes) and as an RGB
    /// PNG (what a clipboard manager might put back).
    fn two_encodings(seed: u8) -> (Vec<u8>, Vec<u8>) {
        let (w, h) = (4u32, 3u32);
        let rgb: Vec<u8> = (0..w * h * 3)
            .map(|i| (i as u8).wrapping_mul(seed))
            .collect();
        let rgba: Vec<u8> = rgb
            .chunks_exact(3)
            .flat_map(|c| [c[0], c[1], c[2], 255])
            .collect();
        let ours = macwin_platform::image::encode_png(&macwin_platform::image::Rgba {
            width: w,
            height: h,
            pixels: rgba,
        })
        .unwrap();
        let mut theirs = Vec::new();
        let mut e = png::Encoder::new(&mut theirs, w, h);
        e.set_color(png::ColorType::Rgb);
        e.set_depth(png::BitDepth::Eight);
        e.write_header().unwrap().write_image_data(&rgb).unwrap();
        assert_ne!(ours, theirs);
        (ours, theirs)
    }

    #[tokio::test]
    async fn re_encoded_copies_of_an_image_keep_its_bytes() {
        let (written, rewritten) = two_encodings(7);
        let (copied, recopied) = two_encodings(11);
        let (cb, user) = FakeClipboard::new();
        let mut limits = limits();
        limits.snapshot.max_image_bytes = 4096;
        let (worker, mut rx) = ClipboardWorker::start(Box::new(cb), limits, POLL).unwrap();
        // macwin writes the peer's image; a clipboard manager re-encodes it.
        worker.write(ClipboardWrite::Image(written.clone()));
        let deadline = Instant::now() + Duration::from_secs(5);
        while user.writes().is_empty() {
            assert!(Instant::now() < deadline, "write not executed");
            tokio::time::sleep(POLL).await;
        }
        user.copy_image(&rewritten);
        assert_eq!(next(&mut rx).await, LocalClipboard::Image(written));
        // The same for an image the user copied.
        user.copy_image(&copied);
        assert_eq!(next(&mut rx).await, LocalClipboard::Image(copied.clone()));
        user.copy_image(&recopied);
        assert_eq!(next(&mut rx).await, LocalClipboard::Image(copied));
        // A different picture, or bytes that are no PNG, pass unchanged.
        let (other, _) = two_encodings(13);
        user.copy_image(&other);
        assert_eq!(next(&mut rx).await, LocalClipboard::Image(other));
        user.copy_image(b"not a png");
        assert_eq!(
            next(&mut rx).await,
            LocalClipboard::Image(b"not a png".to_vec())
        );
    }

    #[tokio::test]
    async fn copied_files_arrive_as_a_manifest_or_too_large() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("small.txt"), b"12345").unwrap();
        std::fs::write(dir.path().join("big.bin"), vec![0u8; 2000]).unwrap();
        let (cb, user) = FakeClipboard::new();
        let (_worker, mut rx) = ClipboardWorker::start(Box::new(cb), limits(), POLL).unwrap();
        user.copy_files(&[dir.path().join("small.txt")]);
        let LocalClipboard::Files(files) = next(&mut rx).await else {
            panic!("expected files")
        };
        assert_eq!(files.entries.len(), 1);
        assert_eq!(
            (files.entries[0].path.as_str(), files.entries[0].size),
            ("small.txt", 5)
        );
        user.copy_files(&[dir.path().join("big.bin")]);
        assert_eq!(
            next(&mut rx).await,
            LocalClipboard::TooLarge { bytes: 2000 }
        );
        user.copy_files(&[dir.path().join("missing")]);
        assert_eq!(next(&mut rx).await, LocalClipboard::Unsupported);
    }
}
