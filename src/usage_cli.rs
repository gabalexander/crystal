//! `crystal usage`: what crystal's processes take, memory and CPU, as the
//! TUI's resources view (`#`) shows it, for a script: a row each session,
//! the biggest first, then the daemon, each process it runs that isn't a
//! session's, the agent kept warm, and what crystal takes itself and all of
//! it together; or the daemon's look as JSON, with those two.

use crate::client;
use crate::output::outln;
use crate::protocol::{Request, Response};
use crate::resources::{self, Resources, Total};
use anyhow::{Result, bail};
use serde::Serialize;
use std::path::Path;

/// What the sessions' rows go by, the biggest first.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum Sort {
    #[default]
    Ram,
    Cpu,
}

/// The look as `--json` prints it: the daemon's, and its totals.
#[derive(Serialize)]
struct Report<'a> {
    #[serde(flatten)]
    resources: &'a Resources,
    /// What crystal takes itself: the daemon and its helpers.
    crystal: Total,
    /// Everything: crystal itself, the sessions and the agent kept warm.
    all: Total,
}

/// Asks the daemon what crystal's processes take, and prints it.
pub fn run(socket: &Path, json: bool, sort: Sort) -> Result<()> {
    // This command isn't the client anyone wants to know about.
    let request = Request::Resources { client: None };
    let resources = match client::ask(socket, &request, false)? {
        Some(Response::Resources(resources)) => resources,
        Some(_) => bail!("the daemon answered something else"),
        None => bail!("no daemon is running on {}", socket.display()),
    };
    if json {
        let report = Report {
            resources: &resources,
            crystal: resources.own(),
            all: resources.all(),
        };
        outln!("{}", serde_json::to_string_pretty(&report)?)?;
        return Ok(());
    }
    crate::print_table(
        ["NAME", "KIND", "PID", "PROCESSES", "RAM", "CPU"],
        &rows(&resources, sort),
    )
}

/// The table's rows: the sessions by `sort`, crystal's own, then the
/// totals. CPU is `-` until the daemon could count it.
fn rows(resources: &Resources, sort: Sort) -> Vec<[String; 6]> {
    let cpu = |cpu: f64| match resources.cpu_over_ms {
        0 => "-".to_string(),
        _ => resources::cpu(cpu),
    };
    let row = |name: &str, kind: &str, pid: Option<u32>, total: Total| {
        [
            name.to_string(),
            kind.to_string(),
            pid.map_or("-".to_string(), |pid| pid.to_string()),
            total.processes.to_string(),
            resources::size(total.bytes),
            cpu(total.cpu),
        ]
    };
    let alone = |name: &str, kind: &str, usage: &resources::Usage| {
        row(name, kind, Some(usage.pid), Total::from(usage))
    };
    let mut sessions: Vec<_> = resources.sessions.iter().collect();
    sessions.sort_by(|a, b| {
        let (a, b) = (&a.usage, &b.usage);
        match sort {
            Sort::Ram => b.bytes.cmp(&a.bytes),
            Sort::Cpu => b.cpu.total_cmp(&a.cpu).then(b.bytes.cmp(&a.bytes)),
        }
    });
    let mut rows: Vec<[String; 6]> = (sessions.iter())
        .map(|session| alone(&session.name, "session", &session.usage))
        .collect();
    rows.push(alone("crystal", "daemon", &resources.daemon));
    for helper in &resources.helpers {
        rows.push(alone(&helper.name, "helper", &helper.usage));
    }
    if let Some(warm) = &resources.warm {
        rows.push(alone("agent kept warm", "warm", warm));
    }
    rows.push(row("crystal itself", "total", None, resources.own()));
    rows.push(row("all", "total", None, resources.all()));
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resources::{Helper, SessionUsage, Usage};

    fn usage(pid: u32, mb: u64, cpu: f64) -> Usage {
        Usage {
            pid,
            bytes: mb << 20,
            cpu,
            processes: 1,
        }
    }

    #[test]
    fn the_sessions_come_first_by_what_they_take_then_crystal_s_own_and_the_totals() {
        let session = |name: &str, pid, mb, cpu| SessionUsage {
            name: name.to_string(),
            usage: usage(pid, mb, cpu),
        };
        let resources = Resources {
            daemon: usage(1, 30, 1.2),
            helpers: vec![Helper {
                name: "git".to_string(),
                usage: usage(5, 10, 20.0),
            }],
            sessions: vec![session("small", 2, 50, 90.0), session("big", 3, 900, 0.5)],
            warm: Some(usage(4, 200, 0.0)),
            cpu_over_ms: 1000,
            ..Resources::default()
        };
        let table = rows(&resources, Sort::Ram);
        let first: Vec<&str> = table.iter().map(|row| row[0].as_str()).collect();
        assert_eq!(
            first,
            [
                "big",
                "small",
                "crystal",
                "git",
                "agent kept warm",
                "crystal itself",
                "all"
            ]
        );
        assert_eq!(table[0], ["big", "session", "3", "1", "900 MB", "0.5%"]);
        assert_eq!(
            table[5],
            ["crystal itself", "total", "-", "2", "40 MB", "21%"]
        );
        assert_eq!(table[6][3..], ["5", "1.2 GB", "112%"]);
        assert_eq!(rows(&resources, Sort::Cpu)[0][0], "small");
        // Before CPU could be counted, the column says so.
        let resources = Resources {
            cpu_over_ms: 0,
            ..resources
        };
        assert_eq!(rows(&resources, Sort::Ram)[0][5], "-");
    }
}
