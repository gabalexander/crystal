//! The TUI's end of `crystal tab` and `crystal pane`: a connection to the
//! daemon that brings in the layout orders the TUI is given, and takes back
//! its answers and, whenever the user does something in it, that it was
//! used, so orders go to the TUI used last, and when its terminal gains and
//! loses the focus. A handover or a restart cuts it, and the TUI offers
//! again at once, saying when it was last used, and whether its terminal
//! has the focus. See [`crate::layout_relay`].

use super::Event;
use crate::events::now_ms;
use crate::layout::{Layout, Relayed, Report};
use crate::protocol::{self, Request, Response};
use anyhow::{Result, bail};
use std::io::BufReader;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// How long the TUI waits to offer again when there's no daemon to take
/// its offer, as over a restart that isn't a handover.
const OFFER_AGAIN_AFTER: Duration = Duration::from_millis(500);

pub struct Link {
    /// Where reports go, while the daemon takes them.
    reports: Arc<Mutex<Option<UnixStream>>>,
    /// When the user last did something here, in milliseconds since the
    /// Unix epoch; opening counts.
    used: Arc<AtomicU64>,
    /// Whether the terminal has the focus, once it has said.
    focused: Arc<Mutex<Option<bool>>>,
}

impl Link {
    /// Offers the daemon at `socket` to take layout orders, on a thread of
    /// its own, and again whenever the daemon comes back. Each order comes
    /// to the event loop as an event.
    pub fn open(socket: &Path, events: Sender<Event>) -> Link {
        let link = Link {
            reports: Arc::default(),
            used: Arc::new(AtomicU64::new(now_ms())),
            focused: Arc::default(),
        };
        thread::spawn({
            let socket = socket.to_path_buf();
            let reports = link.reports.clone();
            let used = link.used.clone();
            let focused = link.focused.clone();
            move || take_orders(&socket, &reports, &used, &focused, &events)
        });
        link
    }

    /// Tells the daemon the user did something here.
    pub fn used(&self) {
        self.used.store(now_ms(), Ordering::Relaxed);
        self.report(&Report::Used);
    }

    /// Tells the daemon the terminal has gained the focus, or lost it.
    pub fn focus(&self, focused: bool) {
        *self.focused.lock().unwrap() = Some(focused);
        self.report(&Report::Focus { focused });
    }

    /// Answers order `id` with the layout it came to, or why it couldn't
    /// be carried out.
    pub fn answer(&self, id: u64, answer: Result<Layout, String>) {
        self.report(&Report::Answer { id, answer });
    }

    /// Sends `report`, if the daemon is there to take it: without one,
    /// there's nobody to tell.
    fn report(&self, report: &Report) {
        if let Some(conn) = self.reports.lock().unwrap().as_ref() {
            let _ = protocol::send(conn, report);
        }
    }
}

/// Takes orders from the daemon at `socket` for as long as the TUI runs,
/// and sends each to `events`. Once the daemon goes, it offers again
/// straight away: over a handover, the offer waits for the next daemon.
fn take_orders(
    socket: &Path,
    reports: &Mutex<Option<UnixStream>>,
    used: &AtomicU64,
    focused: &Mutex<Option<bool>>,
    events: &Sender<Event>,
) {
    loop {
        let Ok((conn, mut orders)) = offer(socket, used.load(Ordering::Relaxed)) else {
            thread::sleep(OFFER_AGAIN_AFTER);
            continue;
        };
        {
            // Held while the focus is said, so a change to it can't come
            // between.
            let focused = focused.lock().unwrap();
            if let Some(focused) = *focused {
                let _ = protocol::send(&conn, &Report::Focus { focused });
            }
            *reports.lock().unwrap() = Some(conn);
        }
        while let Ok(Some(relayed)) = protocol::recv::<Relayed>(&mut orders) {
            if events.send(Event::Layout(relayed)).is_err() {
                return;
            }
        }
        *reports.lock().unwrap() = None;
    }
}

/// Offers the daemon at `socket` to take orders, as a TUI last used at
/// `used`: the connection to report on, and the orders as they come.
fn offer(socket: &Path, used: u64) -> Result<(UnixStream, BufReader<UnixStream>)> {
    let conn = UnixStream::connect(socket)?;
    protocol::send_request(&conn, &Request::TakeLayoutOrders { used })?;
    // One reader for the answer and the orders after it, which may come
    // in the same read.
    let mut orders = BufReader::new(conn.try_clone()?);
    match protocol::recv::<Response>(&mut orders)? {
        Some(Response::Done) => Ok((conn, orders)),
        _ => bail!("the daemon won't give this TUI layout orders"),
    }
}
