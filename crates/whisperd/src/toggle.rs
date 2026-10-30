//! Turns `toggle` requests into one toggle per key press.
//!
//! A held key sends the bound command again and again (niri repeats binds
//! unless they set `repeat=false`; X11 desktops do the same). Taken at face
//! value, that is a rapid start/stop/start/stop, each stop transcribing a
//! sliver of near-silence, which Whisper answers with "Thank you". This was
//! the original niri bug.
//!
//! The timings follow Myna's control trigger (`myna-desktop/src/shortcut/
//! control.rs`), reimplemented here:
//!
//! - a poke at least [`QUIET`] after the previous one toggles at once
//! - a closer poke toggles only if no other poke follows it within
//!   [`FOLLOW`]: repeats come tens of ms apart, a person's second press
//!   does not. That costs a lone quick second press a 200 ms delay.
//! - once a repeat stream is recognised, pokes within [`FOLLOW`] of the
//!   previous one are dropped until the stream pauses
//!
//! What a toggle *means* (start or stop) is decided by the daemon from its
//! current state, so there is no press/release parity to drift out of sync.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::time::Instant;

/// Key repeat starts after the repeat delay (~500–660 ms on X11 desktops;
/// Myna measured the first repeat arriving 574 ms after the press). A poke
/// within this of the previous one may be a repeat.
pub const QUIET: Duration = Duration::from_millis(800);
/// Repeats arrive at most ~93 ms apart (Myna's measurement); a second press
/// is never this close.
pub const FOLLOW: Duration = Duration::from_millis(200);
/// Pokes beyond this many waiting are spam and dropped.
const BACKLOG: usize = 8;

#[derive(Debug, Clone, Copy)]
struct Arrival {
    at: Instant,
    /// Since the previous poke, counting ones dropped by a full backlog.
    gap: Option<Duration>,
}

/// Cheap handle the IPC layer pokes on every `toggle` request.
#[derive(Clone)]
pub struct Poker {
    tx: mpsc::Sender<Arrival>,
    last: Arc<Mutex<Option<Instant>>>,
}

impl Poker {
    /// Never blocks; a full backlog drops the poke.
    pub fn poke(&self) {
        let at = Instant::now();
        let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
        let gap = last.map(|before| at.duration_since(before));
        *last = Some(at);
        let _ = self.tx.try_send(Arrival { at, gap });
    }
}

/// Spawn the filter. `on_toggle` runs once per accepted press.
pub fn spawn<F>(on_toggle: F) -> Poker
where
    F: Fn() -> bool + Send + 'static,
{
    let (tx, rx) = mpsc::channel(BACKLOG);
    tokio::spawn(run(rx, on_toggle));
    Poker {
        tx,
        last: Arc::new(Mutex::new(None)),
    }
}

/// `on_toggle` returns `false` once the receiver is gone, which ends the task.
async fn run<F: Fn() -> bool>(mut rx: mpsc::Receiver<Arrival>, on_toggle: F) {
    let mut streaming = false;
    let mut held: Option<Arrival> = None;
    loop {
        let arrival = match held.take() {
            Some(a) => a,
            None => match rx.recv().await {
                Some(a) => a,
                None => return,
            },
        };
        let close = |a: &Arrival, limit| a.gap.is_some_and(|g| g < limit);

        if !close(&arrival, QUIET) {
            streaming = false;
            if !on_toggle() {
                return;
            }
            continue;
        }
        if streaming && close(&arrival, FOLLOW) {
            continue;
        }
        streaming = false;

        // Candidate press: wait to see whether a repeat follows it.
        match tokio::time::timeout_at(arrival.at + FOLLOW, rx.recv()).await {
            Ok(Some(next)) if close(&next, FOLLOW) => streaming = true,
            Ok(next) => {
                held = next;
                if !on_toggle() {
                    return;
                }
            }
            Err(_timeout) => {
                if !on_toggle() {
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn counter() -> (Poker, Arc<AtomicUsize>) {
        let n = Arc::new(AtomicUsize::new(0));
        let n2 = n.clone();
        let poker = spawn(move || {
            n2.fetch_add(1, Ordering::SeqCst);
            true
        });
        (poker, n)
    }

    async fn ms(n: u64) {
        tokio::time::sleep(Duration::from_millis(n)).await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_single_press_toggles_immediately() {
        let (p, n) = counter();
        p.poke();
        ms(1).await;
        assert_eq!(n.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_held_key_toggles_once() {
        let (p, n) = counter();
        p.poke(); // press
        ms(574).await; // repeat delay
        for _ in 0..20 {
            p.poke(); // repeats, 50 ms apart
            ms(50).await;
        }
        ms(1_000).await;
        assert_eq!(n.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn two_presses_far_apart_toggle_twice() {
        let (p, n) = counter();
        p.poke();
        ms(3_000).await;
        p.poke();
        ms(1).await;
        assert_eq!(n.load(Ordering::SeqCst), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn a_quick_second_press_toggles_after_the_follow_window() {
        let (p, n) = counter();
        p.poke();
        ms(650).await; // stop soon after start: inside QUIET, not a repeat
        p.poke();
        ms(100).await;
        assert_eq!(
            n.load(Ordering::SeqCst),
            1,
            "still waiting to rule out a repeat"
        );
        ms(150).await;
        assert_eq!(n.load(Ordering::SeqCst), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn a_deliberate_double_tap_still_starts_then_stops() {
        // Two pokes are not a repeat stream; only a third close one would be.
        let (p, n) = counter();
        p.poke();
        ms(60).await;
        p.poke();
        ms(1_000).await;
        assert_eq!(n.load(Ordering::SeqCst), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn a_burst_of_close_pokes_after_a_press_is_dropped() {
        let (p, n) = counter();
        p.poke();
        ms(300).await;
        for _ in 0..5 {
            p.poke();
            ms(30).await;
        }
        ms(1_000).await;
        assert_eq!(n.load(Ordering::SeqCst), 1);
    }
}
