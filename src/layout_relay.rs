//! The daemon's side of `crystal tab`, `crystal pane` and `crystal layout`:
//! the TUIs that take layout orders, which of them was used last, and an
//! order passed on to that one with its answer passed back.
//!
//! A TUI offers to take orders as it opens (`Request::TakeLayoutOrders`),
//! and its connection stays open: the daemon writes it each order, a
//! [`Relayed`] line, and the TUI writes back [`Report`] lines, its answers
//! and, whenever the user does something in it, that it was used. A
//! command (`Request::Layout`) waits for the answer, a while at most.
//!
//! With no TUI to pass an order on to, it fails with [`NoTui`], and the
//! daemon carries it out itself, on the tabs the TUIs keep.
//!
//! A TUI also says when its terminal gains and loses the focus, so the
//! daemon knows whether the user is at crystal at all: see
//! [`notify::Presence`].
//!
//! Nothing here is handed over: a handover, or any restart, cuts every
//! TUI's connection, and each offers again at once, saying when it was last
//! used, so the next daemon knows which was used last as they come back.

use crate::events::now_ms;
use crate::layout::{Layout, NO_TUI, Order, Relayed, Report};
use crate::notify::{self, Presence};
use crate::protocol::{self, Response};
use anyhow::{Context, Result, anyhow, bail};
use std::collections::HashMap;
use std::io::BufRead;
use std::os::unix::net::UnixStream;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

/// How long a command waits for the TUI's answer. A TUI answers as soon as
/// it has carried the order out; one that takes longer is stuck.
const ANSWER_WITHIN: Duration = Duration::from_secs(5);

/// How long after the daemon starts a command waits for a TUI to come
/// back, when none has yet: TUIs offer again as soon as the daemon they had
/// has gone, over a handover or a restart.
const TUIS_BACK_WITHIN: Duration = Duration::from_secs(2);

type Answer = Result<Layout, String>;

/// What passing an order on fails with when no TUI takes orders.
#[derive(Debug)]
pub struct NoTui;

impl std::fmt::Display for NoTui {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(NO_TUI)
    }
}

impl std::error::Error for NoTui {}

pub struct Relay {
    state: Mutex<State>,
    /// Told whenever a TUI comes.
    came: Condvar,
    /// When the daemon started.
    started: Instant,
}

#[derive(Default)]
struct State {
    tuis: Vec<Tui>,
    /// How many TUIs have come, which numbers them.
    came: u64,
    /// The number the last order was given.
    last_order: u64,
}

/// A TUI that takes orders.
struct Tui {
    /// Its number, in the order TUIs came.
    id: u64,
    /// Where its orders are written.
    conn: UnixStream,
    /// When it was last used, in milliseconds since the Unix epoch: as it
    /// said when it came, and from then on, when it said it was used.
    used: u64,
    /// The orders it was given that wait on its answer, by number.
    waiting: HashMap<u64, Sender<Answer>>,
    /// Whether its terminal has the focus, once it has said.
    focused: Option<bool>,
}

impl Relay {
    pub fn new() -> Relay {
        Relay {
            state: Mutex::default(),
            came: Condvar::new(),
            started: Instant::now(),
        }
    }

    /// Takes the connection of a TUI that offers to take orders, last used
    /// at `used`, and reads what it reports until it hangs up. The orders
    /// waiting on its answer then fail.
    pub fn serve(&self, conn: &UnixStream, mut input: impl BufRead, used: u64) -> Result<()> {
        let tui = {
            let mut state = self.state.lock().unwrap();
            // Under the lock, so no order is written ahead of this.
            protocol::send(conn, &Response::Done)?;
            state.add(conn.try_clone()?, used)?
        };
        self.came.notify_all();
        while let Ok(Some(report)) = protocol::recv::<Report>(&mut input) {
            let mut state = self.state.lock().unwrap();
            match report {
                Report::Used => state.used(tui, now_ms()),
                Report::Answer { id, answer } => state.answer(tui, id, answer),
                Report::Focus { focused } => {
                    state.focus(tui, focused);
                    notify::set_presence(state.presence());
                }
            }
        }
        let mut state = self.state.lock().unwrap();
        state.remove(tui);
        notify::set_presence(state.presence());
        Ok(())
    }

    /// Passes `order` on to the TUI used last, and waits for the layout it
    /// comes to. Just after the daemon starts, it gives the TUIs a moment
    /// to come back first.
    pub fn pass(&self, order: Order) -> Result<Layout> {
        let (answer, answered) = mpsc::channel();
        let state = self.state.lock().unwrap();
        let back_by = self.started + TUIS_BACK_WITHIN;
        let wait = back_by.saturating_duration_since(Instant::now());
        let (mut state, _) = (self.came)
            .wait_timeout_while(state, wait, |state| state.tuis.is_empty())
            .unwrap();
        let (tui, number) = state.give(order, answer)?;
        drop(state);
        match answered.recv_timeout(ANSWER_WITHIN) {
            Ok(answer) => answer.map_err(|why| anyhow!(why)),
            Err(RecvTimeoutError::Timeout) => {
                self.state.lock().unwrap().forget(tui, number);
                bail!("the TUI didn't answer within {}s", ANSWER_WITHIN.as_secs())
            }
            Err(RecvTimeoutError::Disconnected) => bail!("the TUI closed before it answered"),
        }
    }
}

impl State {
    /// Takes in a TUI that writes to `conn`, last used at `used`.
    fn add(&mut self, conn: UnixStream, used: u64) -> Result<u64> {
        // A TUI that stops reading mustn't keep a command, and the lock,
        // forever.
        conn.set_write_timeout(Some(ANSWER_WITHIN))?;
        self.came += 1;
        self.tuis.push(Tui {
            id: self.came,
            conn,
            used,
            waiting: HashMap::new(),
            focused: None,
        });
        Ok(self.came)
    }

    /// Lets go of the TUI `tui`. The commands waiting on it are told, as
    /// their answers' senders go with it.
    fn remove(&mut self, tui: u64) {
        self.tuis.retain(|held| held.id != tui);
    }

    /// The TUI `tui` was used at `at`.
    fn used(&mut self, tui: u64, at: u64) {
        if let Some(held) = self.find(tui) {
            held.used = held.used.max(at);
        }
    }

    /// The terminal of the TUI `tui` has gained the focus, or lost it.
    fn focus(&mut self, tui: u64, focused: bool) {
        if let Some(held) = self.find(tui) {
            held.focused = Some(focused);
        }
    }

    /// Whether the user is at crystal: a TUI's terminal has the focus, or
    /// every one's has lost it; or, with no TUI or one whose terminal
    /// hasn't said, nobody knows.
    fn presence(&self) -> Presence {
        let focus: Vec<Option<bool>> = self.tuis.iter().map(|tui| tui.focused).collect();
        if focus.contains(&Some(true)) {
            Presence::Here
        } else if !focus.is_empty() && focus.iter().all(|focused| *focused == Some(false)) {
            Presence::Away
        } else {
            Presence::Unknown
        }
    }

    /// Hands the command waiting on order `number` of `tui` its answer.
    fn answer(&mut self, tui: u64, number: u64, answer: Answer) {
        let waiting = self.find(tui).and_then(|held| held.waiting.remove(&number));
        if let Some(waiting) = waiting {
            let _ = waiting.send(answer);
        }
    }

    /// Writes `order` to the TUI used last, of two used at once the one
    /// that came last, for its answer to go to `answer`, and says which TUI
    /// and the order's number.
    fn give(&mut self, order: Order, answer: Sender<Answer>) -> Result<(u64, u64)> {
        self.last_order += 1;
        let number = self.last_order;
        let tui = (self.tuis.iter_mut())
            .max_by_key(|held| (held.used, held.id))
            .ok_or(NoTui)?;
        let relayed = Relayed { id: number, order };
        protocol::send(&tui.conn, &relayed).context("couldn't reach the TUI")?;
        tui.waiting.insert(number, answer);
        Ok((tui.id, number))
    }

    /// Gives up on order `number` of `tui`: its answer, if it ever comes,
    /// goes nowhere.
    fn forget(&mut self, tui: u64, number: u64) {
        if let Some(held) = self.find(tui) {
            held.waiting.remove(&number);
        }
    }

    fn find(&mut self, tui: u64) -> Option<&mut Tui> {
        self.tuis.iter_mut().find(|held| held.id == tui)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::Command;
    use std::io::BufReader;

    fn order() -> Order {
        Order {
            command: Command::Show,
            caller: None,
        }
    }

    /// A TUI taken in, last used at `used`: the daemon's end, given to the
    /// state, and the TUI's, to read orders from.
    fn tui(state: &mut State, used: u64) -> (u64, BufReader<UnixStream>) {
        let (daemon, tui) = UnixStream::pair().unwrap();
        (state.add(daemon, used).unwrap(), BufReader::new(tui))
    }

    fn next_order(tui: &mut BufReader<UnixStream>) -> Relayed {
        protocol::recv(tui).unwrap().unwrap()
    }

    #[test]
    fn an_order_goes_to_the_tui_used_last() {
        let mut state = State::default();
        let (first, mut first_end) = tui(&mut state, 100);
        let (_, mut second_end) = tui(&mut state, 100);
        let (answer, _answered) = mpsc::channel();

        // Used at once, the second came last.
        state.give(order(), answer.clone()).unwrap();
        assert_eq!(next_order(&mut second_end).id, 1);

        state.used(first, 200);
        state.give(order(), answer).unwrap();
        assert_eq!(next_order(&mut first_end).id, 2);
    }

    #[test]
    fn tuis_coming_back_after_a_handover_keep_which_was_used_last() {
        let mut state = State::default();
        let (_, mut used_last) = tui(&mut state, 300);
        let (came_last, _end) = tui(&mut state, 200);
        let (answer, _answered) = mpsc::channel();
        state.give(order(), answer.clone()).unwrap();
        assert_eq!(next_order(&mut used_last).id, 1);
        // A use the daemon heard of late doesn't go back.
        state.used(came_last, 100);
        state.give(order(), answer).unwrap();
        assert_eq!(next_order(&mut used_last).id, 2);
    }

    #[test]
    fn a_command_just_after_the_daemon_starts_waits_for_a_tui_to_come_back() {
        let relay = std::sync::Arc::new(Relay::new());
        let passed = std::thread::spawn({
            let relay = relay.clone();
            move || relay.pass(order())
        });
        std::thread::sleep(Duration::from_millis(100));
        let (daemon, tui) = UnixStream::pair().unwrap();
        std::thread::spawn({
            let relay = relay.clone();
            move || relay.serve(&daemon, BufReader::new(daemon.try_clone().unwrap()), 1)
        });
        let mut tui = BufReader::new(tui);
        let done: Response = protocol::recv(&mut tui).unwrap().unwrap();
        assert!(matches!(done, Response::Done));
        let relayed = next_order(&mut tui);
        let layout = Layout {
            tabs: Vec::new(),
            presence: Presence::Unknown,
        };
        let answer = Report::Answer {
            id: relayed.id,
            answer: Ok(layout.clone()),
        };
        protocol::send(tui.get_ref(), &answer).unwrap();
        assert_eq!(passed.join().unwrap().unwrap(), layout);
    }

    #[test]
    fn an_answer_reaches_the_command_waiting_on_it() {
        let mut state = State::default();
        let (id, mut end) = tui(&mut state, 1);
        let (answer, answered) = mpsc::channel();
        let (tui, number) = state.give(order(), answer).unwrap();
        assert_eq!((tui, number), (id, next_order(&mut end).id));

        state.answer(tui, number, Err("there's no tab 4".into()));
        assert_eq!(answered.recv().unwrap(), Err("there's no tab 4".into()));
    }

    #[test]
    fn the_user_is_away_only_once_every_tui_has_lost_the_focus() {
        let mut state = State::default();
        assert_eq!(state.presence(), Presence::Unknown);
        let (first, _first_end) = tui(&mut state, 1);
        let (second, _second_end) = tui(&mut state, 1);
        assert_eq!(state.presence(), Presence::Unknown);
        state.focus(first, false);
        assert_eq!(
            state.presence(),
            Presence::Unknown,
            "the second hasn't said"
        );
        state.focus(second, true);
        assert_eq!(state.presence(), Presence::Here);
        state.focus(second, false);
        assert_eq!(state.presence(), Presence::Away);
        state.remove(second);
        assert_eq!(state.presence(), Presence::Away);
        state.remove(first);
        assert_eq!(state.presence(), Presence::Unknown);
    }

    #[test]
    fn a_tui_that_goes_fails_the_commands_waiting_on_it() {
        let mut state = State::default();
        let (id, _end) = tui(&mut state, 1);
        let (answer, answered) = mpsc::channel();
        state.give(order(), answer).unwrap();
        state.remove(id);
        assert!(answered.recv().is_err());

        let (answer, _answered) = mpsc::channel();
        let err = state.give(order(), answer).unwrap_err();
        assert!(err.is::<NoTui>(), "{err}");
    }
}
