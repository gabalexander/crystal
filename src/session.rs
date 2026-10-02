//! A program running in a PTY of its own.

use crate::protocol::{SessionInfo, State};
use anyhow::Result;
use portable_pty::{CommandBuilder, ExitStatus, PtySize, native_pty_system};
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// How long a stopped session gets to exit after its hang-up before it's
/// killed outright.
pub const STOP_GRACE: Duration = Duration::from_secs(2);

pub struct Session {
    pub name: String,
    command: Vec<String>,
    cwd: PathBuf,
    pid: Option<u32>,
    state: Arc<Mutex<State>>,
}

impl Session {
    pub fn spawn(
        name: String,
        command: Vec<String>,
        cwd: PathBuf,
        env: &[(&str, &str)],
    ) -> Result<Session> {
        let pty = native_pty_system().openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        let mut builder = CommandBuilder::new(&command[0]);
        builder.args(&command[1..]);
        builder.cwd(&cwd);
        for (key, value) in env {
            builder.env(key, value);
        }
        let mut child = pty.slave.spawn_command(builder)?;
        // Only the child may hold the terminal's other end, so that its exit
        // ends the output.
        drop(pty.slave);

        // Nothing shows the output yet, but someone has to read it: a full
        // PTY buffer blocks the program on its next write.
        let mut output = pty.master.try_clone_reader()?;
        thread::spawn(move || io::copy(&mut output, &mut io::sink()));

        let pid = child.process_id();
        let state = Arc::new(Mutex::new(State::Running));
        let exit = state.clone();
        thread::spawn(move || {
            let ended = match child.wait() {
                Ok(status) => ended(&status),
                Err(_) => State::Exited { code: 1 },
            };
            *exit.lock().unwrap() = ended;
        });

        Ok(Session {
            name,
            command,
            cwd,
            pid,
            state,
        })
    }

    pub fn is_running(&self) -> bool {
        *self.state.lock().unwrap() == State::Running
    }

    pub fn info(&self) -> SessionInfo {
        SessionInfo {
            name: self.name.clone(),
            command: self.command.clone(),
            cwd: self.cwd.clone(),
            pid: self.pid,
            state: self.state.lock().unwrap().clone(),
        }
    }

    /// Hangs up on everything the session started, the way closing a
    /// terminal window does, and kills whatever is still there after
    /// [`STOP_GRACE`].
    pub fn stop(&self) {
        let Some(pid) = self.pid.filter(|_| self.is_running()) else {
            return;
        };
        signal_group(pid, libc::SIGHUP);
        let state = self.state.clone();
        thread::spawn(move || {
            thread::sleep(STOP_GRACE);
            if *state.lock().unwrap() == State::Running {
                signal_group(pid, libc::SIGKILL);
            }
        });
    }
}

fn ended(status: &ExitStatus) -> State {
    match status.signal() {
        // macOS spells it "Terminated: 15"; keep the name.
        Some(signal) => State::Signaled {
            signal: signal.split(':').next().unwrap_or(signal).to_string(),
        },
        None => State::Exited {
            code: status.exit_code(),
        },
    }
}

/// The child leads its own session and process group, so signalling the
/// group reaches whatever it started too.
fn signal_group(pid: u32, signal: libc::c_int) {
    // SAFETY: kill only sends a signal. The caller has checked the child
    // hasn't been reaped yet, so the group id still belongs to it.
    unsafe {
        libc::kill(-(pid as libc::pid_t), signal);
    }
}
