//! The event log, and the daemon's [`Bus`], which every event goes through.
//!
//! The log is the `events` table of the daemon's database (see
//! [`crate::db`]), as docket keeps its own: a row an event, under its
//! `seq`. The daemon is its one writer: [`Bus::emit`] numbers each event,
//! writes it down, and sends it on to every [`Subscription`] whose filter
//! takes it: the clients streaming events over the socket, and the plugins'
//! hooks. Anything outside the daemon that does something worth an event
//! tells the daemon (`Request::Emit`). Anyone can read the log: `crystal
//! events` does, with no daemon at all.
//!
//! The log keeps `[events] keep_days` days, and never more than [`MOST`]
//! events: the daemon prunes it as it starts, every [`PRUNE_EVERY`] after,
//! and as soon as it holds too many. An event that can't be written down
//! still reaches its subscribers: the log is never worth stopping for.

use crate::db::Db;
use crate::events::{Event, Filter, Since, now_ms};
use anyhow::Result;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How many events the log may hold, whatever their age, before the oldest
/// go, until it holds half that.
const MOST: u64 = 50_000;

/// How often the daemon prunes the log.
pub const PRUNE_EVERY: Duration = Duration::from_secs(60 * 60);

/// Events a subscriber may fall behind by before it's dropped, rather
/// than hold up the daemon.
const BACKLOG: usize = 4096;

/// Every event in the log of the daemon at `socket` that `filter` takes,
/// from `since`, the oldest first.
pub fn read(socket: &Path, filter: &Filter, since: Since) -> Result<Vec<Event>> {
    let events = Db::open(socket)?.events(since)?;
    Ok(events
        .into_iter()
        .filter(|event| filter.matches(event))
        .collect())
}

/// The daemon's side of events: numbering them, writing them down, and
/// handing them to the subscribers.
pub struct Bus {
    socket: PathBuf,
    state: Mutex<BusState>,
}

struct BusState {
    /// Where the events are written down: `None` when the database
    /// couldn't be opened, which leaves them only sent on.
    db: Option<Db>,
    /// The `seq` of the latest event, 0 before the first.
    last: u64,
    /// How many events the log holds.
    count: u64,
    subscribers: Vec<Subscriber>,
    next_id: u64,
}

struct Subscriber {
    id: u64,
    filter: Filter,
    feed: SyncSender<Arc<Event>>,
}

/// The events a subscriber's filter takes, from when it subscribed. Its
/// feed ends if it falls too far behind: it has lost events, and has to
/// subscribe again and catch up from the log.
pub struct Subscription {
    pub id: u64,
    /// The `seq` of the latest event before it: the log has that one and
    /// those before it, and the feed gets every one after.
    pub seq: u64,
    pub feed: Receiver<Arc<Event>>,
}

impl Bus {
    /// The bus of the daemon at `socket`, numbering on from the latest
    /// event its log has ever had.
    pub fn new(socket: &Path) -> Bus {
        let db = Db::open(socket)
            .inspect_err(|err| eprintln!("crystal daemon: couldn't open the event log: {err:#}"))
            .ok();
        let known = |read: fn(&Db) -> Result<u64>| {
            let db = db.as_ref()?;
            read(db)
                .inspect_err(|err| {
                    eprintln!("crystal daemon: couldn't read the event log: {err:#}")
                })
                .ok()
        };
        let last = known(Db::latest_event).unwrap_or(0);
        let count = known(Db::event_count).unwrap_or(0);
        Bus {
            socket: socket.to_path_buf(),
            state: Mutex::new(BusState {
                db,
                last,
                count,
                subscribers: Vec::new(),
                next_id: 0,
            }),
        }
    }

    /// Numbers `event`, writes it in the log, and sends it to each
    /// subscriber that wants it. All of it under one lock, so the log, the
    /// feeds and the numbers keep one order.
    pub fn emit(&self, mut event: Event) {
        let mut guard = self.state.lock().unwrap();
        let state = &mut *guard;
        state.last += 1;
        event.seq = state.last;
        event.at = now_ms();
        if let Some(db) = &state.db {
            match db.add_event(&event) {
                Ok(()) => state.count += 1,
                Err(err) => eprintln!(
                    "crystal daemon: couldn't write {} in the event log: {err:#}",
                    event.kind.name()
                ),
            }
        }
        let event = Arc::new(event);
        state.subscribers.retain(|subscriber| {
            !subscriber.filter.matches(&event) || subscriber.feed.try_send(event.clone()).is_ok()
        });
        if state.count > MOST {
            state.prune(None, MOST);
        }
    }

    /// Starts sending the events `filter` takes, from the next one on.
    pub fn subscribe(&self, filter: Filter) -> Subscription {
        let mut state = self.state.lock().unwrap();
        let (feed, receiver) = mpsc::sync_channel(BACKLOG);
        let id = state.next_id;
        state.next_id += 1;
        state.subscribers.push(Subscriber { id, filter, feed });
        Subscription {
            id,
            seq: state.last,
            feed: receiver,
        }
    }

    pub fn unsubscribe(&self, id: u64) {
        let mut state = self.state.lock().unwrap();
        state.subscribers.retain(|subscriber| subscriber.id != id);
    }

    /// The events in the log that `filter` takes, from `since` up to the
    /// one numbered `through`: what a subscriber catches up on. Read on a
    /// connection of their own, so the daemon goes on emitting meanwhile.
    pub fn replay(&self, filter: &Filter, since: Since, through: u64) -> Vec<Event> {
        let logged = read(&self.socket, filter, since).unwrap_or_else(|err| {
            eprintln!("crystal daemon: couldn't read the event log: {err:#}");
            Vec::new()
        });
        logged
            .into_iter()
            .filter(|event| event.seq <= through)
            .collect()
    }

    /// Holds the bus, and its connection to the database, until what's
    /// given back is dropped: for a handover, which closes it with the
    /// exec, so that no event is halfway into the log then.
    pub fn hold(&self) -> impl Sized + '_ {
        self.state.lock().unwrap()
    }

    /// Takes the events older than `keep_days` days out of the log, none
    /// with 0, and the oldest of the rest while it holds too many.
    pub fn prune(&self, keep_days: u32) {
        let cutoff = (keep_days > 0).then(|| {
            let kept = u64::from(keep_days) * 24 * 60 * 60 * 1000;
            now_ms().saturating_sub(kept)
        });
        self.state.lock().unwrap().prune(cutoff, MOST);
    }
}

impl BusState {
    /// Takes the events from before `cutoff`, in milliseconds since the
    /// Unix epoch, out of the log; then, if it still holds more than
    /// `most`, the oldest, until it holds half that.
    fn prune(&mut self, cutoff: Option<u64>, most: u64) {
        let Some(db) = &self.db else {
            return;
        };
        let pruned = (|| {
            if let Some(cutoff) = cutoff {
                db.delete_events_before(cutoff)?;
            }
            let count = db.event_count()?;
            if count <= most {
                return Ok(count);
            }
            db.keep_newest_events(most / 2)?;
            db.event_count()
        })();
        match pruned {
            Ok(count) => self.count = count,
            Err(err) => eprintln!("crystal daemon: couldn't prune the event log: {err:#}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::Kind;

    fn bus() -> (tempfile::TempDir, PathBuf, Bus) {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("crystal.sock");
        let bus = Bus::new(&socket);
        (dir, socket, bus)
    }

    fn event(kind: Kind) -> Event {
        Event::about_project(kind, PathBuf::from("/code/app"))
    }

    fn logged(socket: &Path, since: Since) -> Vec<u64> {
        let events = read(socket, &Filter::default(), since).unwrap();
        events.iter().map(|event| event.seq).collect()
    }

    #[test]
    fn each_event_is_numbered_written_down_and_sent_to_whoever_wants_it() {
        let (_dir, socket, bus) = bus();
        let backlog_only = Filter {
            kinds: vec!["backlog.*".into()],
            ..Filter::default()
        };
        let subscription = bus.subscribe(backlog_only.clone());
        assert_eq!(subscription.seq, 0);
        bus.emit(event(Kind::MemoryAdded));
        bus.emit(event(Kind::BacklogAdded));

        let sent = subscription.feed.try_recv().unwrap();
        assert_eq!((sent.seq, sent.kind), (2, Kind::BacklogAdded));
        assert!(sent.at > 0);
        assert!(subscription.feed.try_recv().is_err());

        assert_eq!(logged(&socket, Since::Seq(0)), [1, 2]);
        assert_eq!(logged(&socket, Since::Seq(1)), [2]);
        let replayed = bus.replay(&backlog_only, Since::Seq(0), 1);
        assert!(replayed.is_empty(), "only what the filter takes, up to 1");
    }

    #[test]
    fn numbering_goes_on_from_the_log_a_daemon_before_left() {
        let (_dir, socket, bus) = bus();
        bus.emit(event(Kind::BacklogAdded));
        bus.emit(event(Kind::BacklogClosed));
        let again = Bus::new(&socket);
        assert_eq!(again.subscribe(Filter::default()).seq, 2);
    }

    #[test]
    fn a_subscriber_too_far_behind_is_dropped_rather_than_holding_things_up() {
        let (_dir, _socket, bus) = bus();
        let subscription = bus.subscribe(Filter::default());
        for _ in 0..=BACKLOG {
            bus.emit(event(Kind::BacklogAdded));
        }
        let got = subscription.feed.iter().count();
        assert_eq!(got, BACKLOG, "the feed ends once it's dropped");
    }

    #[test]
    fn pruning_lets_old_events_go_and_never_the_numbering() {
        let (_dir, socket, bus) = bus();
        for _ in 0..6 {
            bus.emit(event(Kind::BacklogAdded));
        }
        let mut state = bus.state.lock().unwrap();
        state.prune(None, 4);
        assert_eq!(state.count, 2);
        drop(state);
        assert_eq!(logged(&socket, Since::Seq(0)), [5, 6]);
        bus.prune(30);
        assert_eq!(logged(&socket, Since::Seq(0)), [5, 6]);

        // Every event gone, the numbers still go on, in this daemon and
        // the next.
        bus.state.lock().unwrap().prune(Some(now_ms() + 1), 4);
        assert!(logged(&socket, Since::Seq(0)).is_empty());
        assert_eq!(Bus::new(&socket).subscribe(Filter::default()).seq, 6);
        bus.emit(event(Kind::BacklogAdded));
        assert_eq!(logged(&socket, Since::Seq(0)), [7]);
    }
}
