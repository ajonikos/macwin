//! The clipboard thread of a session: it owns the clipboard backend, polls
//! its change count, reports local changes and writes the peer's texts.
//!
//! All backend calls happen on this one thread: the Windows clipboard may
//! block for a moment when another process holds it, and AppKit objects are
//! best kept off the async runtime. Changes caused by the worker's own
//! writes are not reported (it remembers the change count after writing).

use std::sync::mpsc as std_mpsc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use macwin_platform::{ClipboardBackend, LocalClipboard, PlatformError};
use tokio::sync::mpsc;
use tracing::{debug, warn};

/// How often the change count is checked.
pub const POLL_INTERVAL: Duration = Duration::from_millis(250);
/// Local changes queued towards the driver.
const EVENT_QUEUE: usize = 16;

enum Command {
    Write(String),
    Stop,
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
        max_text_bytes: usize,
        poll: Duration,
    ) -> Result<(ClipboardWorker, mpsc::Receiver<LocalClipboard>), PlatformError> {
        let baseline = backend.change_count()?;
        let (commands, command_rx) = std_mpsc::channel();
        let (events_tx, events) = mpsc::channel(EVENT_QUEUE);
        let join = thread::Builder::new()
            .name("macwin-clipboard".into())
            .spawn(move || {
                run(
                    backend,
                    baseline,
                    max_text_bytes,
                    poll,
                    command_rx,
                    events_tx,
                )
            })
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

    /// Replaces the local clipboard with `text` (asynchronously).
    pub fn write(&self, text: String) {
        let _ = self.commands.send(Command::Write(text));
    }

    /// Stops the thread and waits for it (at most one poll interval).
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

fn run(
    backend: Box<dyn ClipboardBackend>,
    baseline: u64,
    max_text_bytes: usize,
    poll: Duration,
    commands: std_mpsc::Receiver<Command>,
    events: mpsc::Sender<LocalClipboard>,
) {
    let mut seen = baseline;
    loop {
        match commands.recv_timeout(poll) {
            Ok(Command::Write(text)) => match backend.write_text(&text) {
                Ok(count) => seen = count,
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
        match backend.snapshot(max_text_bytes) {
            Ok(snap) => {
                seen = snap.change_count;
                if events.blocking_send(snap.content).is_err() {
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

    use macwin_platform::testing::FakeClipboard;

    use super::*;

    const POLL: Duration = Duration::from_millis(5);

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
        let (mut worker, mut rx) = ClipboardWorker::start(Box::new(cb), 100, POLL).unwrap();
        user.copy_text("hello");
        assert_eq!(next(&mut rx).await, LocalClipboard::Text("hello".into()));
        worker.write("from peer".into());
        let deadline = Instant::now() + Duration::from_secs(5);
        while user.writes().is_empty() {
            assert!(Instant::now() < deadline, "write not executed");
            tokio::time::sleep(POLL).await;
        }
        assert_eq!(user.text().as_deref(), Some("from peer"));
        user.copy_image();
        // The own write produced no event: the next one is the image.
        assert_eq!(next(&mut rx).await, LocalClipboard::NoText);
        user.copy_sensitive("pw");
        assert_eq!(next(&mut rx).await, LocalClipboard::Sensitive);
        worker.stop();
        user.copy_text("after stop");
        tokio::time::sleep(POLL * 4).await;
        assert!(rx.try_recv().is_err());
    }
}
