//! The warm agent, `[sessions] warm_agent`: one Claude Code started and
//! waiting where the TUI's selection is, as the new-session panel would
//! start it there, so that a new session started the same way starts at
//! once, its first prompt typed into the agent as it takes it over. It's
//! started as that session would be, crystal's notes and all, but for its
//! first prompt, so what the project's memory tells it goes by its
//! command's words alone, not its task's; and a session started any other
//! way starts as ever. It's a real Claude Code the whole time, which takes
//! what an idle one does: started again once it's [`RECYCLE_AFTER`] old
//! and asked for, so what it was told stays fresh; let go once nobody has
//! asked for it in [`KEPT_FOR`], once it's that old, as soon as the
//! setting goes off, and before a handover, which it isn't part of. Never
//! in the list, never told of, until a session takes it over. Adapted from
//! docket's prewarm pool.

use super::{
    Daemon, check_dir, claude_tools, exists, keep_scrollback, launch_notes, name_for, names_itself,
    new_id, new_task_info, settings,
};
use crate::agents;
use crate::catalog;
use crate::config::Config;
use crate::env;
use crate::protocol::{AgentEvent, Conversation, NewSession, Response, TaskBrief, WarmAgent};
use crate::session::{Session, Term};
use crate::tasks;
use crate::typing;
use anyhow::{Context, Result, ensure};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

/// How long after it started a warm agent takes a prompt: Claude Code says
/// it has started a moment before its prompt takes keys. A session that
/// takes one over sooner has its prompt typed once it's this old.
pub(super) const READY_AFTER: Duration = Duration::from_secs(3);

/// How old a warm agent asked for again is started again at.
const RECYCLE_AFTER: Duration = Duration::from_secs(10 * 60);

/// How long a warm agent is kept without being asked for again, and how
/// old it gets at most: a TUI asks every few minutes while it's open.
const KEPT_FOR: Duration = Duration::from_secs(15 * 60);

/// What the warm agent's terminal is called, as its program hears it.
const NAME: &str = "warm-agent";

/// The warm agent: its session, what it was started as, and when.
pub(super) struct Spare {
    session: Session,
    warm: WarmAgent,
    /// Whether it was told of closing its task as it started: tasks were
    /// on, and the session that takes it over was to be a task.
    tasked: bool,
    started: Instant,
    asked: Instant,
}

impl Spare {
    /// Whether `new` would start as this agent was started, but for its
    /// first prompt, and the agent has said it's up, at its prompt.
    fn fits(&self, new: &NewSession, config: &Config) -> bool {
        let tasked = new.task.is_some() && tasks::enabled(config);
        let ready = self.session.is_running() && self.session.at_prompt();
        ready
            && new.cwd == self.warm.cwd
            && new.env == self.warm.env
            && new.brief.is_empty()
            && tasked == self.tasked
            && catalog::without_first_prompt(&new.command) == self.warm.command
    }

    /// Whether it's what `warm` asks for, and young enough to keep.
    fn keeps(&self, warm: &WarmAgent) -> bool {
        self.warm == *warm && self.session.is_running() && self.started.elapsed() < RECYCLE_AFTER
    }
}

impl Daemon {
    /// Keeps an agent warm as `warm` says: the one there, asked for again,
    /// when it's the same and young, or else a new one in its place. With
    /// the setting off, or for anything but Claude Code, none.
    pub(super) fn warm(&self, warm: WarmAgent) -> Result<Response> {
        let config = settings();
        let wanted =
            config.sessions.warm_agent && agents::program_name(&warm.command) == Some("claude");
        let old = {
            let mut spare = self.spare.lock().unwrap();
            if let Some(kept) = spare.as_mut().filter(|kept| wanted && kept.keeps(&warm)) {
                kept.asked = Instant::now();
                return Ok(Response::Done);
            }
            spare.take()
        };
        if let Some(old) = old {
            old.session.stop();
        }
        if !wanted {
            return Ok(Response::Done);
        }
        // Got ready with nothing held: what it's told is looked for in the
        // project's memory, which can take a while.
        let tasked = warm.task && tasks::enabled(&config);
        let boot = Boot::of(&self.socket, &warm, tasked, &config)?;
        // Started with the agent kept warm held, so that it's there to hear
        // its hooks, which wait for that, as soon as it says anything.
        let raced = {
            let mut spare = self.spare.lock().unwrap();
            let now = Instant::now();
            let session = boot.spawn(&warm)?;
            spare.replace(Spare {
                session,
                warm,
                tasked,
                started: now,
                asked: now,
            })
        };
        // Two asked for at once: the last stays.
        if let Some(raced) = raced {
            raced.session.stop();
        }
        Ok(Response::Done)
    }

    /// The warm agent, when `new` can take it over and it's ready to be.
    pub(super) fn take_spare(&self, new: &NewSession) -> Option<Spare> {
        let mut spare = self.spare.lock().unwrap();
        let fits = spare
            .as_ref()
            .is_some_and(|kept| kept.fits(new, &settings()));
        fits.then(|| spare.take()).flatten()
    }

    /// Keeps up with the warm agent, as the keep-up loop goes round: its
    /// screen read, what it did meanwhile no news; and let go once it has
    /// ended, nobody has asked for it lately, it's too old, or the settings
    /// turn it off.
    pub(super) fn keep_spare(&self, settings: &Config) {
        let mut spare = self.spare.lock().unwrap();
        let Some(kept) = spare.as_mut() else {
            return;
        };
        kept.session.check();
        kept.session.take_changes();
        let gone = !settings.sessions.warm_agent
            || !kept.session.is_running()
            || kept.asked.elapsed() > KEPT_FOR
            || kept.started.elapsed() > KEPT_FOR;
        if gone && let Some(old) = spare.take() {
            old.session.stop();
        }
    }

    /// Hears what the warm agent's hooks say, when the session `id` is it:
    /// `false` when it's another.
    pub(super) fn spare_heard(
        &self,
        id: &str,
        event: AgentEvent,
        conversation: Option<Conversation>,
        model: Option<&str>,
    ) -> bool {
        let mut spare = self.spare.lock().unwrap();
        let Some(kept) = spare.as_mut().filter(|kept| kept.session.id == id) else {
            return false;
        };
        let session = &mut kept.session;
        if let Some(conversation) = conversation {
            session.set_hooked_conversation("claude", conversation);
        }
        if let Some(model) = model {
            session.heard_model(model);
        }
        session.on_agent_event(event);
        true
    }

    /// Lets the warm agent go at once, for a handover, which it isn't part
    /// of, or a shutdown that stops the sessions.
    pub(super) fn drop_spare(&self) {
        let old = self.spare.lock().unwrap().take();
        if let Some(old) = old {
            old.session.kill_now();
        }
    }

    /// The warm agent's program, for the RAM view.
    pub(super) fn spare_pid(&self) -> Option<u32> {
        self.spare.lock().unwrap().as_ref()?.session.running_pid()
    }
}

/// Takes `spare` over as the session `new` asks for, at the end of
/// `sessions`, its first prompt typed in, and gives back its name. One that
/// can't be, as when the name it's given is taken, is let go.
pub(super) fn adopt(sessions: &mut Vec<Session>, spare: Spare, new: NewSession) -> Result<String> {
    let ready_in = READY_AFTER.saturating_sub(spare.started.elapsed());
    let mut session = spare.session;
    let config = settings();
    let program = new.command.first().context("no command to run")?;
    let agent_names = new.agent_names && names_itself(new.name.as_deref(), &new.command, &config);
    let named = name_for(sessions, new.name, new.task.as_deref(), program, &config);
    let (name, named_after_program) = match named {
        Ok(named) => named,
        Err(err) => {
            session.stop();
            return Err(err);
        }
    };
    let prompt = catalog::first_prompt_at(&new.command).map(|at| new.command[at].clone());
    session.take_over(name.clone(), new.command);
    if named_after_program {
        session.mark_named_after_program();
    }
    if agent_names {
        session.let_agent_name();
    }
    session.set_about(&new.brief);
    if let Some(goal) = new.task.filter(|_| tasks::enabled(&config)) {
        session.give_task(new_task_info(goal, false, new.backlog, new.brief));
    }
    if let Some(prompt) = prompt {
        type_prompt(session.term(), prompt, ready_in);
    }
    sessions.push(session);
    Ok(name)
}

/// What a warm agent is started with, got ready before it starts: its id,
/// its arguments, crystal's notes among them, and its environment; and the
/// entries of its project's memory the notes show it.
struct Boot {
    id: String,
    argv: Vec<String>,
    env: BTreeMap<String, String>,
    remembered: Vec<u64>,
}

impl Boot {
    /// Claude Code as `warm` says, told what a session started that way
    /// would be, but for a first prompt: of closing its task when `tasked`.
    fn of(socket: &Path, warm: &WarmAgent, tasked: bool, config: &Config) -> Result<Boot> {
        let command = &warm.command;
        let program = command.first().context("no command to run")?;
        check_dir(&warm.cwd)?;
        ensure!(
            exists(program, &warm.cwd, warm.env.get("PATH")),
            "command not found: {program}"
        );
        let id = new_id();
        let mut env = env::for_session(&warm.env, NAME, &id, socket);
        env.insert(agents::HOOKED.into(), "claude".into());
        let crystal = std::env::current_exe()?;
        // Its task is what's typed into it: the notes say only how to close
        // it.
        let task = tasked.then_some("");
        let (notes, remembered) = launch_notes(
            socket,
            &warm.cwd,
            command,
            task,
            &TaskBrief::default(),
            config,
        );
        let argv = agents::argv(command, &crystal, None, None, &notes);
        let tools = claude_tools(socket, &warm.cwd, command, &crystal, config);
        let argv = agents::with_options(argv, &tools);
        Ok(Boot {
            id,
            argv,
            env,
            remembered,
        })
    }

    /// Starts it, as `warm` asked for it.
    fn spawn(self, warm: &WarmAgent) -> Result<Session> {
        keep_scrollback();
        let mut session = Session::spawn(
            self.id,
            NAME.to_string(),
            warm.command.clone(),
            &self.argv,
            warm.cwd.clone(),
            &self.env,
            None,
        )?;
        session.recalled().launched(&self.remembered);
        Ok(session)
    }
}

/// Types `prompt` into the agent at `term` as its first, once it has been
/// up `ready_in` more: a paste, then Enter on its own a moment after, off
/// the thread that asked.
fn type_prompt(term: Arc<Term>, prompt: String, ready_in: Duration) {
    thread::spawn(move || {
        thread::sleep(ready_in);
        let pasted = typing::keystrokes(&prompt, term.wants_bracketed_paste());
        if term.write(&pasted).is_ok() {
            thread::sleep(typing::ENTER_PAUSE);
            let _ = term.write(typing::ENTER);
        }
    });
}
