//! `crystal wiki serve` and `crystal wiki open`: the wikis of a crystal
//! server's projects served to the browser, on 127.0.0.1 only, with the
//! chat that answers questions about their code (see [`crate::wiki_ask`]).
//!
//! It's `std::net` and a thread a connection, each answering one request
//! and closing: a page, its files and mermaid ([`crate::wiki_site`]), a
//! wiki's `wiki.json`, how its build stands, a question streamed back as
//! server-sent events, and a file opened in the user's editor at a line.
//! Nothing but those is read: a URL names a page's file by its name among
//! the files built in, and a project by its directory's name among the
//! wikis, and a file to open must be in the project's repository. A request
//! whose `Host` isn't 127.0.0.1 or localhost is refused, which keeps a site
//! that rebinds its name to this machine out; and a question or an open
//! from another site (its `Origin`, or what `Sec-Fetch-Site` says) is
//! refused too.
//!
//! `crystal wiki open` runs one in the background when none is running for
//! the server: a helper process, `crystal wiki serve --helper`, cut loose
//! from the terminal, that stops once the server's daemon has. Each server
//! writes which port it's on, its pid and its crystal into `serve.json` in
//! the server's wikis' directory, where `open` finds it; one of another
//! crystal, after an upgrade, is stopped and started again.

use crate::client;
use crate::clipboard;
use crate::config::Config;
use crate::layout;
use crate::links;
use crate::output::{errln, outln};
use crate::plugins;
use crate::project;
use crate::protocol::{Request as DaemonRequest, Response};
use crate::state;
use crate::tui;
use crate::wiki::{self, BUILD_FILE, WIKI_FILE};
use crate::wiki_ask::{self, Question};
use crate::wiki_site::{self, MERMAID, Mermaid};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::borrow::Cow;
use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

/// The port a server takes when it's free: another is chosen when it isn't.
pub const DEFAULT_PORT: u16 = 7347;

/// Where a server says which port it's on, in the wikis' directory.
const SERVING_FILE: &str = "serve.json";

/// What the helper `open` starts writes, in the wikis' directory.
const LOG_FILE: &str = "serve.log";

/// The longest a request's line and headers may be, and its body.
const MAX_HEAD: usize = 16 * 1024;
const MAX_BODY: usize = 64 * 1024;

/// How long a connection may take to send its request.
const READ_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a write to a page may hang before the page is taken as gone.
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);

/// How often the helper looks for its daemon, which it stops without: once
/// it's missed it twice in a row, so a restart doesn't stop it.
const DAEMON_CHECK: Duration = Duration::from_secs(30);

/// This crystal's version, which a server says so `open` knows its own.
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// A server running, as `serve.json` says it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Serving {
    pid: u32,
    port: u16,
    crystal: String,
}

/// What a server serves from.
struct Server {
    socket: PathBuf,
    /// The server's wikis' directory, a directory a project.
    wikis: PathBuf,
    port: u16,
    mermaid: Mermaid,
}

/// `crystal wiki serve`: serves every wiki of the server at `socket` on
/// `port`, or [`DEFAULT_PORT`] or else a free one, until it's stopped; with
/// `open`, opening the browser on the page of the project `dir` is in. As
/// the `helper` `open` starts, it's cut loose from the terminal, says
/// nothing but in its log, and stops once the server's daemon has.
pub fn serve(
    socket: &Path,
    port: Option<u16>,
    dir: Option<PathBuf>,
    open: bool,
    helper: bool,
) -> Result<()> {
    plugins::ensure_enabled(&Config::load()?, "wiki")?;
    if helper {
        // SAFETY: setsid has no preconditions.
        unsafe {
            libc::setsid();
        }
    }
    let listener = bind(port)?;
    let port = listener.local_addr()?.port();
    let wikis = state::wikis_dir(socket);
    fs::create_dir_all(&wikis).with_context(|| format!("couldn't make {}", wikis.display()))?;
    let serving = Serving {
        pid: std::process::id(),
        port,
        crystal: VERSION.to_string(),
    };
    write_serving(&wikis, &serving)?;
    let server = Arc::new(Server {
        socket: socket.to_path_buf(),
        wikis,
        port,
        mermaid: Mermaid::new(),
    });
    // Ready before the first page asks for it.
    let early = server.clone();
    thread::spawn(move || {
        if let Err(why) = early.mermaid.get() {
            errln!("the diagrams are left as their source: {why}");
        }
    });
    if helper {
        let server = server.clone();
        thread::spawn(move || stop_with_daemon(&server));
    }
    let home = format!("http://127.0.0.1:{port}/");
    if helper {
        errln!("serving the wikis at {home}");
    } else {
        outln!("serving the wikis at {home} (ctrl+c stops it)")?;
        for (name, path) in listed(&server.wikis) {
            outln!("  {name}: {home}{}", path.trim_start_matches('/'))?;
        }
    }
    if open {
        let page = page_of(socket, dir)?;
        show(&format!("http://127.0.0.1:{port}{page}"), port)?;
    }
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let server = server.clone();
        thread::spawn(move || server.handle(stream));
    }
    Ok(())
}

/// A listener on 127.0.0.1: on `port` if it's given, or else on
/// [`DEFAULT_PORT`], or a free port when that's taken.
fn bind(port: Option<u16>) -> Result<TcpListener> {
    let at = |port| TcpListener::bind((Ipv4Addr::LOCALHOST, port));
    match port {
        Some(port) => at(port).with_context(|| format!("couldn't listen on port {port}")),
        None => at(DEFAULT_PORT)
            .or_else(|_| at(0))
            .context("couldn't listen on 127.0.0.1"),
    }
}

/// Stops the helper `server` is, once its daemon is gone: no daemon, no
/// crystal server to serve the wikis of.
fn stop_with_daemon(server: &Server) {
    let mut missed = 0;
    loop {
        thread::sleep(DAEMON_CHECK);
        missed = match UnixStream::connect(&server.socket) {
            Ok(_) => 0,
            Err(_) => missed + 1,
        };
        if missed >= 2 {
            errln!("the daemon has stopped, and so does the wikis' server");
            forget_serving(&server.wikis);
            std::process::exit(0);
        }
    }
}

/// Writes `serving` into `wikis`, whole or not at all.
fn write_serving(wikis: &Path, serving: &Serving) -> Result<()> {
    let path = wikis.join(SERVING_FILE);
    let partial = wikis.join(format!("{SERVING_FILE}.{}", serving.pid));
    fs::write(&partial, serde_json::to_vec(serving)?)
        .with_context(|| format!("couldn't write {}", partial.display()))?;
    fs::rename(&partial, &path).with_context(|| format!("couldn't write {}", path.display()))
}

fn read_serving(wikis: &Path) -> Option<Serving> {
    serde_json::from_slice(&fs::read(wikis.join(SERVING_FILE)).ok()?).ok()
}

/// Takes out `serve.json` when it's this process's.
fn forget_serving(wikis: &Path) {
    if read_serving(wikis).is_some_and(|serving| serving.pid == std::process::id()) {
        let _ = fs::remove_file(wikis.join(SERVING_FILE));
    }
}

/// `crystal wiki open`: opens the browser on the wiki of the project `dir`
/// is in, or on the list of wikis when it has none, starting a server in
/// the background first when none is running. Over ssh, it says the
/// address, and how to reach it from the user's own machine.
pub fn open(socket: &Path, dir: Option<PathBuf>) -> Result<()> {
    plugins::ensure_enabled(&Config::load()?, "wiki")?;
    let page = page_of(socket, dir)?;
    // The helper lives as long as the daemon does: one runs from now on.
    client::ask(socket, &DaemonRequest::List, true)?;
    let port = match running(socket) {
        Some(port) => port,
        None => start_helper(socket)?,
    };
    show(&format!("http://127.0.0.1:{port}{page}"), port)
}

/// The path of the page of the project `dir` is in, or of the list of
/// wikis, saying so, when it has none.
fn page_of(socket: &Path, dir: Option<PathBuf>) -> Result<String> {
    let dir = match dir {
        Some(dir) => dir,
        None => std::env::current_dir().context("couldn't tell the current directory")?,
    };
    let project = project::of(&dir);
    let wiki = wiki::dir(socket, &project.path);
    if wiki.join(WIKI_FILE).is_file() {
        let key = wiki.file_name().unwrap_or_default().to_string_lossy();
        return Ok(format!("/p/{key}/"));
    }
    errln!(
        "{} has no wiki yet: `crystal wiki build` writes one; showing the others",
        project.name
    );
    Ok("/".to_string())
}

/// Opens `url` in the browser and says it; over ssh, says how to forward
/// its `port` from the user's machine, then it. Last, for whoever reads
/// only the last line, like the TUI.
fn show(url: &str, port: u16) -> Result<()> {
    if clipboard::remote() {
        outln!(
            "crystal runs on another machine: forward the port from yours, then open the link \
             there:\n  ssh -L {port}:127.0.0.1:{port} {}",
            tui::window::hostname()
        )?;
    } else {
        links::open(url)?;
    }
    outln!("{url}")?;
    Ok(())
}

/// The port of the server running for `socket`, if one of this crystal is.
/// One of another crystal is stopped, for this one to take its place.
fn running(socket: &Path) -> Option<u16> {
    let wikis = state::wikis_dir(socket);
    let serving = read_serving(&wikis)?;
    // SAFETY: kill with no signal only asks whether the process is there.
    if unsafe { libc::kill(serving.pid as i32, 0) } != 0 {
        return None;
    }
    let says = ask_server(serving.port, "/api/server");
    let ours = says.as_ref().is_some_and(|says| {
        says["crystal"] == VERSION && says["wikis"].as_str() == Some(&*wikis.to_string_lossy())
    });
    if ours {
        return Some(serving.port);
    }
    if says.is_some() {
        // SAFETY: as above; it's a wikis' server of this crystal server.
        unsafe {
            libc::kill(serving.pid as i32, libc::SIGTERM);
        }
    }
    None
}

/// What the server on `port` answers to a GET of `path`, as JSON.
fn ask_server(port: u16, path: &str) -> Option<Value> {
    let address = (Ipv4Addr::LOCALHOST, port).into();
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(1)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    )
    .ok()?;
    let mut answer = Vec::new();
    stream.read_to_end(&mut answer).ok()?;
    let at = answer.windows(4).position(|w| w == b"\r\n\r\n")?;
    serde_json::from_slice(&answer[at + 4..]).ok()
}

/// Starts a server in the background for `socket`, and gives its port once
/// it's listening.
fn start_helper(socket: &Path) -> Result<u16> {
    let wikis = state::wikis_dir(socket);
    fs::create_dir_all(&wikis).with_context(|| format!("couldn't make {}", wikis.display()))?;
    let log_path = wikis.join(LOG_FILE);
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    // Not waited on: it outlives us, and init reaps it.
    let child = Command::new(std::env::current_exe()?)
        .arg("--socket")
        .arg(socket)
        .args(["wiki", "serve", "--helper"])
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .spawn()
        .context("couldn't start the wikis' server")?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Some(serving) = read_serving(&wikis).filter(|serving| serving.pid == child.id()) {
            return Ok(serving.port);
        }
        thread::sleep(Duration::from_millis(20));
    }
    bail!("the wikis' server didn't start; see {}", log_path.display())
}

/// A request, as far as the server reads it.
#[derive(Debug, Default)]
struct Request {
    method: String,
    /// The path, its escapes undone, without its query.
    path: String,
    query: String,
    /// The headers, their names in lower case.
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        let found = self.headers.iter().find(|(header, _)| header == name);
        found.map(|(_, value)| value.as_str())
    }

    /// The value of `name` in the query, its escapes undone.
    fn param(&self, name: &str) -> Option<String> {
        self.query.split('&').find_map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (decode(key, true)? == name).then(|| decode(value, true))?
        })
    }
}

/// Reads a request from `reader`, or the status that refuses it.
fn read_request(reader: &mut impl BufRead, out: &mut impl Write) -> Result<Request, u16> {
    let mut head = Vec::new();
    loop {
        let before = head.len();
        let read = reader
            .by_ref()
            .take((MAX_HEAD + 1 - head.len()) as u64)
            .read_until(b'\n', &mut head)
            .map_err(|_| 400u16)?;
        if read == 0 {
            return Err(400);
        }
        if head.len() > MAX_HEAD {
            return Err(431);
        }
        if head[before..].trim_ascii().is_empty() && before > 0 {
            break;
        }
    }
    let head = String::from_utf8(head).map_err(|_| 400u16)?;
    let mut lines = head.lines();
    let mut first = lines.next().unwrap_or_default().split(' ');
    let (method, target) = (
        first.next().unwrap_or_default(),
        first.next().unwrap_or_default(),
    );
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let mut request = Request {
        method: method.to_string(),
        path: decode(path, false).ok_or(400u16)?,
        query: query.to_string(),
        ..Request::default()
    };
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            request
                .headers
                .push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
        }
    }
    if request.header("transfer-encoding").is_some() {
        return Err(411);
    }
    let length: usize = match request.header("content-length") {
        Some(length) => length.parse().map_err(|_| 400u16)?,
        None => 0,
    };
    if length > MAX_BODY {
        return Err(413);
    }
    if length > 0 {
        if request
            .header("expect")
            .is_some_and(|expect| expect.eq_ignore_ascii_case("100-continue"))
        {
            let _ = out.write_all(b"HTTP/1.1 100 Continue\r\n\r\n");
        }
        request.body = vec![0; length];
        reader.read_exact(&mut request.body).map_err(|_| 400u16)?;
    }
    Ok(request)
}

/// `text` with its `%XX` escapes undone, and in a query its `+`s spaces;
/// `None` for an escape that isn't one, or what isn't UTF-8.
fn decode(text: &str, query: bool) -> Option<String> {
    let mut bytes = Vec::with_capacity(text.len());
    let mut rest = text.bytes();
    while let Some(byte) = rest.next() {
        match byte {
            b'%' => {
                let hex = [rest.next()?, rest.next()?];
                bytes.push(u8::from_str_radix(std::str::from_utf8(&hex).ok()?, 16).ok()?);
            }
            b'+' if query => bytes.push(b' '),
            byte => bytes.push(byte),
        }
    }
    String::from_utf8(bytes).ok()
}

/// What a request asks for.
#[derive(Debug, PartialEq, Eq)]
enum Route<'a> {
    /// The list of wikis: a page, or JSON for whoever asks for it.
    Home,
    /// The list of wikis, as JSON.
    Projects,
    /// Which crystal this is, for `open`.
    Server,
    /// One of the page's files, or mermaid.
    Asset(&'a str),
    /// A wiki's page.
    Page(&'a str),
    /// A wiki's page without its last `/`, which its links need.
    Slash(&'a str),
    Wiki(&'a str),
    Status(&'a str),
    Open(&'a str),
    Ask(&'a str),
}

/// What `method` and `path` ask for, or the status that refuses them. A
/// path that steps out of where it is (`..`) is refused, though nothing
/// would be read by it.
fn route<'a>(method: &str, path: &'a str) -> Result<Route<'a>, u16> {
    let Some(path) = path.strip_prefix('/') else {
        return Err(400);
    };
    let parts: Vec<&str> = path.split('/').collect();
    let stepping = |part: &&str| matches!(*part, "." | "..") || part.contains(['\\', '\0']);
    if parts.iter().any(stepping) {
        return Err(400);
    }
    let route = match parts.as_slice() {
        [""] => Route::Home,
        ["projects.json"] => Route::Projects,
        ["api", "server"] => Route::Server,
        ["assets", name] | ["p", _, "assets", name] if !name.is_empty() => Route::Asset(name),
        ["p", key] if is_key(key) => Route::Slash(key),
        ["p", key, ""] if is_key(key) => Route::Page(key),
        ["p", key, "wiki.json"] if is_key(key) => Route::Wiki(key),
        ["p", key, "api", "status"] if is_key(key) => Route::Status(key),
        ["p", key, "api", "open"] if is_key(key) => Route::Open(key),
        ["p", key, "api", "ask"] if is_key(key) => Route::Ask(key),
        _ => return Err(404),
    };
    let wanted = if matches!(route, Route::Ask(_)) {
        "POST"
    } else {
        "GET"
    };
    if method != wanted {
        return Err(405);
    }
    Ok(route)
}

/// Whether `key` could be a wiki's directory's name.
fn is_key(key: &str) -> bool {
    !key.is_empty()
        && !key.starts_with('.')
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// Whether `host`, a `Host` header, names this machine as the server
/// listens on it: 127.0.0.1 or localhost, on any port.
fn is_local_host(host: Option<&str>) -> bool {
    let Some(host) = host else {
        return false;
    };
    let name = match host.rsplit_once(':') {
        Some((name, port)) if port.chars().all(|c| c.is_ascii_digit()) => name,
        _ => host,
    };
    name == "127.0.0.1" || name.eq_ignore_ascii_case("localhost")
}

/// Whether `origin` is the server's own, on `port`.
fn is_own_origin(origin: &str, port: u16) -> bool {
    origin == format!("http://127.0.0.1:{port}") || origin == format!("http://localhost:{port}")
}

/// Why `request`, which does something, can't be taken from where it came:
/// another site's page, by its `Origin` or `Sec-Fetch-Site`; and a POST
/// must say its origin, as browsers do.
fn foreign(request: &Request, port: u16) -> Option<&'static str> {
    match request.header("origin") {
        Some(origin) if !is_own_origin(origin, port) => return Some("another site's page"),
        None if request.method == "POST" => return Some("a page that doesn't say its origin"),
        _ => {}
    }
    match request.header("sec-fetch-site") {
        Some("same-origin" | "none") | None => None,
        Some(_) => Some("another site's page"),
    }
}

/// An answer to a request, but for a question's, which streams.
struct Reply {
    status: u16,
    content_type: &'static str,
    body: Cow<'static, [u8]>,
    headers: Vec<(&'static str, String)>,
}

impl Reply {
    fn bytes(content_type: &'static str, body: impl Into<Cow<'static, [u8]>>) -> Reply {
        Reply {
            status: 200,
            content_type,
            body: body.into(),
            headers: Vec::new(),
        }
    }

    fn json(value: &Value) -> Reply {
        Reply::bytes("application/json", value.to_string().into_bytes())
    }

    fn text(status: u16, text: impl Into<String>) -> Reply {
        Reply {
            status,
            ..Reply::bytes("text/plain; charset=utf-8", text.into().into_bytes())
        }
    }

    fn empty() -> Reply {
        Reply {
            status: 204,
            ..Reply::bytes("text/plain", Vec::new())
        }
    }

    fn redirect(to: String) -> Reply {
        Reply {
            status: 301,
            headers: vec![("Location", to)],
            ..Reply::bytes("text/plain", Vec::new())
        }
    }

    fn with(mut self, name: &'static str, value: &str) -> Reply {
        self.headers.push((name, value.to_string()));
        self
    }

    fn write(&self, out: &mut impl Write) -> io::Result<()> {
        let mut head = format!("HTTP/1.1 {} {}\r\n", self.status, reason(self.status));
        if self.status != 204 {
            head.push_str(&format!(
                "Content-Type: {}\r\nContent-Length: {}\r\n",
                self.content_type,
                self.body.len()
            ));
        }
        if !self
            .headers
            .iter()
            .any(|(name, _)| *name == "Cache-Control")
        {
            head.push_str("Cache-Control: no-cache\r\n");
        }
        for (name, value) in &self.headers {
            head.push_str(&format!("{name}: {value}\r\n"));
        }
        head.push_str(SECURITY_HEADERS);
        out.write_all(head.as_bytes())?;
        out.write_all(&self.body)?;
        out.flush()
    }
}

/// What every answer says: never sniffed for another type, never framed by
/// another site, never telling a link's site where it was followed from,
/// and the connection closed after it.
const SECURITY_HEADERS: &str = "X-Content-Type-Options: nosniff\r\nX-Frame-Options: DENY\r\n\
     Referrer-Policy: no-referrer\r\nConnection: close\r\n\r\n";

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        204 => "No Content",
        301 => "Moved Permanently",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        411 => "Length Required",
        413 => "Content Too Large",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Unknown",
    }
}

impl Server {
    /// Answers the one request `stream` sends, and closes it.
    fn handle(&self, stream: TcpStream) {
        let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
        let _ = stream.set_write_timeout(Some(WRITE_TIMEOUT));
        let _ = stream.set_nodelay(true);
        let Ok(read) = stream.try_clone() else {
            return;
        };
        let mut out = stream;
        let mut reader = BufReader::new(read);
        let request = match read_request(&mut reader, &mut out) {
            Ok(request) => request,
            Err(status) => {
                let _ = Reply::text(status, reason(status)).write(&mut out);
                return;
            }
        };
        let _ = self.answer(&request, &mut out);
    }

    fn answer(&self, request: &Request, out: &mut TcpStream) -> io::Result<()> {
        if !is_local_host(request.header("host")) {
            let why = "crystal's wikis answer to 127.0.0.1 and localhost only";
            return Reply::text(403, why).write(out);
        }
        let route = match route(&request.method, &request.path) {
            Ok(route) => route,
            Err(status) => return Reply::text(status, reason(status)).write(out),
        };
        if matches!(route, Route::Ask(_) | Route::Open(_))
            && let Some(from) = foreign(request, self.port)
        {
            return Reply::text(403, format!("refused: it came from {from}")).write(out);
        }
        let reply = match route {
            Route::Home if wants_html(request) => Reply::bytes(
                "text/html; charset=utf-8",
                home_page(&self.wikis).into_bytes(),
            ),
            Route::Home | Route::Projects => Reply::json(&Value::Array(projects(&self.wikis))),
            Route::Server => Reply::json(&json!({
                "crystal": VERSION,
                "wikis": self.wikis.to_string_lossy(),
            })),
            Route::Asset(name) => self.asset(name),
            Route::Slash(key) => Reply::redirect(format!("/p/{key}/")),
            Route::Page(key) => match self.wiki_file(key).is_file() {
                true => Reply::bytes("text/html; charset=utf-8", wiki_site::index()),
                false => Reply::text(404, format!("there's no wiki called {key}")),
            },
            Route::Wiki(key) => match fs::read(self.wiki_file(key)) {
                Ok(bytes) => Reply::bytes("application/json", bytes),
                Err(_) => Reply::text(404, format!("there's no wiki called {key}")),
            },
            Route::Status(key) => match self.wiki(key) {
                Some(wiki) => Reply::json(&status(&self.wikis.join(key), &wiki)),
                None => Reply::text(404, format!("there's no wiki called {key}")),
            },
            Route::Open(key) => self.open(key, request),
            Route::Ask(key) => return self.ask(key, request, out),
        };
        reply.write(out)
    }

    fn wiki_file(&self, key: &str) -> PathBuf {
        self.wikis.join(key).join(WIKI_FILE)
    }

    /// The wiki called `key`, read.
    fn wiki(&self, key: &str) -> Option<Value> {
        serde_json::from_slice(&fs::read(self.wiki_file(key)).ok()?).ok()
    }

    /// One of the page's files, or mermaid, downloaded first if need be.
    fn asset(&self, name: &str) -> Reply {
        if name == MERMAID.name {
            return match self.mermaid.get().and_then(|path| {
                fs::read(&path).map_err(|err| format!("couldn't read {}: {err}", path.display()))
            }) {
                Ok(bytes) => Reply::bytes(wiki_site::content_type(name), bytes)
                    .with("Cache-Control", "max-age=86400"),
                Err(why) => Reply::text(503, format!("mermaid isn't here: {why}")),
            };
        }
        match wiki_site::page_file(name) {
            Some(bytes) => Reply::bytes(wiki_site::content_type(name), bytes),
            None => Reply::text(404, format!("the page has no file called {name}")),
        }
    }

    /// `/api/open`: the file `path` in the wiki's repository opened in the
    /// user's editor, at `line`.
    fn open(&self, key: &str, request: &Request) -> Reply {
        let Some(root) = self.wiki(key).and_then(|wiki| root_of(&wiki)) else {
            return Reply::text(404, format!("there's no wiki called {key}"));
        };
        let path = request.param("path").unwrap_or_default();
        let Some(file) = file_in(&root, &path) else {
            return Reply::text(404, format!("there's no file {path} in {}", root.display()));
        };
        let line = request
            .param("line")
            .and_then(|line| line.parse::<usize>().ok())
            .filter(|line| *line > 0);
        match edit(&self.socket, &root, &file, line) {
            Ok(()) => Reply::empty(),
            Err(err) => Reply::text(500, format!("{err:#}")),
        }
    }

    /// `/api/ask`: the question in `request`'s body, answered as it comes.
    fn ask(&self, key: &str, request: &Request, out: &mut TcpStream) -> io::Result<()> {
        let question: Question = match serde_json::from_slice(&request.body) {
            Ok(question) => question,
            Err(err) => {
                return Reply::text(400, format!("that isn't a question: {err}")).write(out);
            }
        };
        let Some(wiki) = self.wiki(key) else {
            return Reply::text(404, format!("there's no wiki called {key}")).write(out);
        };
        out.write_all(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                 Cache-Control: no-store\r\n{SECURITY_HEADERS}"
            )
            .as_bytes(),
        )?;
        match root_of(&wiki) {
            Some(root) => wiki_ask::answer(&question, &wiki, &root, out),
            None => {
                let at = wiki["repo"]["root"].as_str().unwrap_or("where it was");
                let why = format!("the project isn't at {at} any more");
                wiki_ask::send(out, &wiki_ask::Said::Error(why))
            }
        }
    }
}

/// Whether whoever asked wants a page rather than JSON: a browser going
/// there, rather than a page fetching it.
fn wants_html(request: &Request) -> bool {
    request
        .header("accept")
        .is_some_and(|accept| accept.contains("text/html"))
}

/// The repository a wiki was made from, while it's there.
fn root_of(wiki: &Value) -> Option<PathBuf> {
    let root = PathBuf::from(wiki["repo"]["root"].as_str()?);
    root.is_dir().then_some(root)
}

/// The file at `path`, from `root`, once it's a file in it: not above it,
/// nor out of it through a link.
fn file_in(root: &Path, path: &str) -> Option<PathBuf> {
    let relative = Path::new(path);
    let plain = relative
        .components()
        .all(|part| matches!(part, Component::Normal(_) | Component::CurDir));
    if path.is_empty() || !plain {
        return None;
    }
    let root = fs::canonicalize(root).ok()?;
    let file = fs::canonicalize(root.join(relative)).ok()?;
    (file.starts_with(&root) && file.is_file()).then_some(file)
}

/// The editors with a window of their own, which a click in the browser
/// can start as they are: a terminal's editor needs a terminal, which a
/// crystal session gives it.
const WINDOWED: &[&str] = &[
    "code",
    "code-insiders",
    "codium",
    "cursor",
    "windsurf",
    "zed",
    "subl",
    "gvim",
    "mvim",
];

/// Whether `program` is an editor with a window of its own.
fn windowed(program: &str) -> bool {
    let name = Path::new(program).file_name().unwrap_or_default();
    WINDOWED.iter().any(|editor| name == *editor)
}

/// The user's editor, for a file opened from the browser: `$VISUAL`, or
/// else what the TUI opens files with.
fn editor() -> Result<Vec<String>> {
    match std::env::var("VISUAL") {
        Ok(visual) if !visual.trim().is_empty() => tui::command_line::parse(&visual)
            .map_err(|err| anyhow::anyhow!("$VISUAL: {err}"))
            .and_then(|command| match command.is_empty() {
                true => bail!("$VISUAL is empty"),
                false => Ok(command),
            }),
        _ => tui::editor(),
    }
}

/// Opens `file`, in the repository at `root`, in the user's editor at
/// `line`: one with a window of its own started as it is, and one that runs
/// in a terminal in a session of its own, as the TUI opens a file, brought
/// to the front in the TUI used last.
fn edit(socket: &Path, root: &Path, file: &Path, line: Option<usize>) -> Result<()> {
    let command = tui::editor_at(editor()?, file.display().to_string(), line);
    if windowed(&command[0]) {
        let mut child = Command::new(&command[0])
            .args(&command[1..])
            .current_dir(root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .with_context(|| format!("couldn't start {}", command[0]))?;
        thread::spawn(move || child.wait());
        return Ok(());
    }
    let taken = match client::ask(socket, &DaemonRequest::List, true)? {
        Some(Response::Sessions { sessions }) => sessions,
        _ => Vec::new(),
    };
    let base = file.file_name().unwrap_or_default().to_string_lossy();
    let name = tui::free_name(&base.replace(char::is_whitespace, "-"), &taken);
    let name = client::new_session(socket, Some(name), root.to_path_buf(), command)?;
    let socket = socket.to_path_buf();
    // The page isn't kept waiting for a TUI to answer.
    thread::spawn(move || {
        let focus = layout::Command::Focus {
            session: name,
            raise: true,
        };
        let _ = client::lay_out(&socket, focus);
    });
    Ok(())
}

/// How the wiki in `dir` stands: its build going on, as `build.json` says,
/// when it was made, and whether its branch has moved on since.
fn status(dir: &Path, wiki: &Value) -> Value {
    let build: Option<Value> = fs::read(dir.join(BUILD_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok());
    let build = build.unwrap_or(Value::Null);
    json!({
        // A build that was stopped never wrote down that it had: the lock
        // it held says for sure.
        "building": build["building"].as_bool().unwrap_or(false) && wiki::book::building(dir),
        "progress": build["progress"].as_str(),
        "updated": wiki["generated"]["at"].as_str(),
        "stale": stale(wiki),
    })
}

/// Whether the branch a wiki was made from has moved on from its commit,
/// as far as git can say.
fn stale(wiki: &Value) -> bool {
    let repo = &wiki["repo"];
    let (Some(root), Some(commit)) = (repo["root"].as_str(), repo["commit"].as_str()) else {
        return false;
    };
    let branch = match repo["branch"].as_str() {
        Some(branch) => format!("refs/heads/{branch}^{{commit}}"),
        None => "HEAD".to_string(),
    };
    let head = Command::new("git")
        .args(["-C", root, "rev-parse", "--verify", "--quiet", &branch])
        .stderr(Stdio::null())
        .output();
    match head {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).trim() != commit,
        _ => false,
    }
}

/// The wikis in `wikis`, each with where its page is, by name.
fn projects(wikis: &Path) -> Vec<Value> {
    let Ok(entries) = fs::read_dir(wikis) else {
        return Vec::new();
    };
    let mut listed: Vec<Value> = entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let key = entry.file_name().to_string_lossy().into_owned();
            if !is_key(&key) {
                return None;
            }
            let wiki: Value =
                serde_json::from_slice(&fs::read(entry.path().join(WIKI_FILE)).ok()?).ok()?;
            let repo = &wiki["repo"];
            Some(json!({
                "key": key,
                "name": repo["name"].as_str().unwrap_or(&key),
                "root": repo["root"],
                "commit": repo["commit"],
                "branch": repo["branch"],
                "updated": wiki["generated"]["at"],
                "url": format!("/p/{key}/"),
            }))
        })
        .collect();
    listed.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    listed
}

/// The wikis' names and pages, for saying where they are.
fn listed(wikis: &Path) -> Vec<(String, String)> {
    projects(wikis)
        .iter()
        .map(|project| {
            let name = project["name"].as_str().unwrap_or_default().to_string();
            (
                name,
                project["url"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

/// The page `/` shows a browser: the wikis, each a link to its page.
fn home_page(wikis: &Path) -> String {
    let rows: String = projects(wikis)
        .iter()
        .map(|project| {
            let text = |key: &str| escape(project[key].as_str().unwrap_or_default());
            let commit: String = text("commit").chars().take(8).collect();
            format!(
                "<li><a href=\"{}\">{}</a><span>{} · {}</span></li>",
                text("url"),
                text("name"),
                text("root"),
                commit
            )
        })
        .collect();
    let list = match rows.is_empty() {
        true => "<p>No wikis yet: <code>crystal wiki build</code> in a project writes its own.</p>"
            .to_string(),
        false => format!("<ul>{rows}</ul>"),
    };
    format!(
        "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>Wikis</title><style>{HOME_STYLE}</style></head>\
         <body><main><h1>Wikis</h1>{list}</main></body></html>\n"
    )
}

const HOME_STYLE: &str = "body{margin:0;padding:16px;background:#000;color:#fff;\
     font:14px/28px system-ui,sans-serif}main{max-width:720px;margin:0 auto;background:#131314;\
     border-radius:16px;padding:16px 24px}h1{font-weight:400;font-size:26px}ul{list-style:none;\
     padding:0}li{display:flex;flex-direction:column;padding:8px 0}a{color:#aecbfa;font-size:16px}\
     span,p{color:#9aa0a6}code{background:#36373a;border-radius:4px;padding:4px}";

/// `text` made safe to put in HTML.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(raw: &str) -> Result<Request, u16> {
        read_request(&mut raw.as_bytes(), &mut Vec::new())
    }

    #[test]
    fn a_request_is_read_with_its_headers_query_and_body() {
        let request = read(
            "POST /p/app-1/api/ask?x=1 HTTP/1.1\r\nHost: 127.0.0.1:7347\r\n\
             Content-Length: 5\r\nOrigin: http://127.0.0.1:7347\r\n\r\nhello",
        )
        .unwrap();
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/p/app-1/api/ask");
        assert_eq!(request.query, "x=1");
        assert_eq!(request.header("host"), Some("127.0.0.1:7347"));
        assert_eq!(request.header("origin"), Some("http://127.0.0.1:7347"));
        assert_eq!(request.body, b"hello");
    }

    #[test]
    fn a_request_too_big_or_chunked_is_refused() {
        let long = format!("GET / HTTP/1.1\r\nX: {}\r\n\r\n", "a".repeat(MAX_HEAD));
        assert_eq!(read(&long).unwrap_err(), 431);
        let big = format!(
            "POST / HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
            MAX_BODY + 1
        );
        assert_eq!(read(&big).unwrap_err(), 413);
        let chunked = "POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n";
        assert_eq!(read(chunked).unwrap_err(), 411);
        assert_eq!(read("GET / HTTP/1.1\r\n").unwrap_err(), 400, "cut short");
        assert_eq!(read("GET /%zz HTTP/1.1\r\n\r\n").unwrap_err(), 400);
    }

    #[test]
    fn a_query_s_escapes_are_undone() {
        let request = read("GET /p/a/api/open?path=src%2Fmy+file.rs&line=12 HTTP/1.1\r\n\r\n");
        let request = request.unwrap();
        assert_eq!(request.param("path").as_deref(), Some("src/my file.rs"));
        assert_eq!(request.param("line").as_deref(), Some("12"));
        assert_eq!(request.param("end"), None);
    }

    #[test]
    fn each_path_goes_to_its_route_and_nothing_steps_out() {
        assert_eq!(route("GET", "/"), Ok(Route::Home));
        assert_eq!(route("GET", "/projects.json"), Ok(Route::Projects));
        assert_eq!(route("GET", "/assets/app.js"), Ok(Route::Asset("app.js")));
        assert_eq!(
            route("GET", "/p/app-1/assets/app.js"),
            Ok(Route::Asset("app.js"))
        );
        assert_eq!(route("GET", "/p/app-1"), Ok(Route::Slash("app-1")));
        assert_eq!(route("GET", "/p/app-1/"), Ok(Route::Page("app-1")));
        assert_eq!(route("GET", "/p/app-1/wiki.json"), Ok(Route::Wiki("app-1")));
        assert_eq!(
            route("GET", "/p/app-1/api/status"),
            Ok(Route::Status("app-1"))
        );
        assert_eq!(route("GET", "/p/app-1/api/open"), Ok(Route::Open("app-1")));
        assert_eq!(route("POST", "/p/app-1/api/ask"), Ok(Route::Ask("app-1")));
        assert_eq!(route("GET", "/p/app-1/api/ask"), Err(405));
        assert_eq!(route("POST", "/p/app-1/wiki.json"), Err(405));
        assert_eq!(route("GET", "/assets/../../etc/passwd"), Err(400));
        assert_eq!(route("GET", "/p/../wiki.json"), Err(400));
        assert_eq!(route("GET", "/p/.hidden/wiki.json"), Err(404));
        assert_eq!(route("GET", "/assets/a\\b"), Err(400));
        assert_eq!(route("GET", "/etc/passwd"), Err(404));
        assert_eq!(route("GET", "/assets/"), Err(404));
    }

    #[test]
    fn only_this_machine_s_names_are_answered() {
        assert!(is_local_host(Some("127.0.0.1:7347")));
        assert!(is_local_host(Some("localhost:7347")));
        assert!(is_local_host(Some("LOCALHOST")));
        assert!(!is_local_host(Some("evil.example:7347")));
        assert!(!is_local_host(Some("127.0.0.1.evil.example")));
        assert!(!is_local_host(None));
    }

    #[test]
    fn what_does_something_must_come_from_the_server_s_own_page() {
        let request = |method: &str, headers: &[(&str, &str)]| Request {
            method: method.to_string(),
            headers: (headers.iter())
                .map(|(name, value)| (name.to_string(), value.to_string()))
                .collect(),
            ..Request::default()
        };
        let own = ("origin", "http://127.0.0.1:7347");
        assert_eq!(foreign(&request("POST", &[own]), 7347), None);
        assert_eq!(
            foreign(
                &request("POST", &[("origin", "http://localhost:7347")]),
                7347
            ),
            None
        );
        assert!(
            foreign(&request("POST", &[own]), 8000).is_some(),
            "another port"
        );
        assert!(foreign(&request("POST", &[]), 7347).is_some());
        assert!(
            foreign(
                &request("POST", &[("origin", "https://evil.example")]),
                7347
            )
            .is_some()
        );
        assert_eq!(foreign(&request("GET", &[]), 7347), None, "typed in");
        let fetched = request("GET", &[("sec-fetch-site", "same-origin")]);
        assert_eq!(foreign(&fetched, 7347), None);
        let embedded = request("GET", &[("sec-fetch-site", "cross-site")]);
        assert!(foreign(&embedded, 7347).is_some());
    }

    #[test]
    fn a_file_to_open_must_be_in_the_repository() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
        fs::write(dir.path().join("secret"), "no\n").unwrap();
        std::os::unix::fs::symlink(dir.path().join("secret"), root.join("link")).unwrap();
        let real = fs::canonicalize(root.join("src/main.rs")).unwrap();
        assert_eq!(file_in(&root, "src/main.rs"), Some(real.clone()));
        assert_eq!(file_in(&root, "./src/main.rs"), Some(real));
        assert_eq!(file_in(&root, "../secret"), None);
        assert_eq!(file_in(&root, "src/../../secret"), None);
        assert_eq!(
            file_in(&root, &dir.path().join("secret").display().to_string()),
            None
        );
        assert_eq!(file_in(&root, "link"), None, "out through a link");
        assert_eq!(file_in(&root, "src"), None, "a directory");
        assert_eq!(file_in(&root, ""), None);
        assert_eq!(file_in(&root, "gone.rs"), None);
    }

    #[test]
    fn an_editor_with_a_window_starts_as_it_is() {
        assert!(windowed("code"));
        assert!(windowed("/usr/local/bin/zed"));
        assert!(!windowed("nvim"));
        assert!(!windowed("vi"));
    }

    #[test]
    fn a_reply_says_its_length_type_and_that_it_closes() {
        let mut out = Vec::new();
        Reply::text(404, "gone").write(&mut out).unwrap();
        let out = String::from_utf8(out).unwrap();
        assert!(out.starts_with("HTTP/1.1 404 Not Found\r\n"), "{out}");
        assert!(out.contains("Content-Type: text/plain; charset=utf-8\r\n"));
        assert!(out.contains("Content-Length: 4\r\n"));
        assert!(out.contains("X-Content-Type-Options: nosniff\r\n"));
        assert!(out.ends_with("Connection: close\r\n\r\ngone"));
        let mut out = Vec::new();
        Reply::empty().write(&mut out).unwrap();
        assert!(!String::from_utf8(out).unwrap().contains("Content-Length"));
    }

    #[test]
    fn the_list_has_each_wiki_by_name_and_the_page_escapes_them() {
        let wikis = tempfile::tempdir().unwrap();
        let write = |key: &str, wiki: Value| {
            fs::create_dir_all(wikis.path().join(key)).unwrap();
            fs::write(wikis.path().join(key).join(WIKI_FILE), wiki.to_string()).unwrap();
        };
        write(
            "zeta-1",
            json!({"repo": {"name": "acme/zeta", "root": "/z", "commit": "abc"},
                   "generated": {"at": "2026-10-09T12:00:00Z"}}),
        );
        write(
            "alpha-2",
            json!({"repo": {"name": "<b>alpha</b>", "root": "/a", "commit": "def"}}),
        );
        fs::create_dir_all(wikis.path().join("empty-3")).unwrap();
        let listed = projects(wikis.path());
        let names: Vec<&str> = listed.iter().map(|p| p["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["<b>alpha</b>", "acme/zeta"]);
        assert_eq!(listed[1]["url"], "/p/zeta-1/");
        assert_eq!(listed[1]["updated"], "2026-10-09T12:00:00Z");
        let page = home_page(wikis.path());
        assert!(
            page.contains("<a href=\"/p/zeta-1/\">acme/zeta</a>"),
            "{page}"
        );
        assert!(page.contains("&lt;b&gt;alpha&lt;/b&gt;"));
        assert!(home_page(&wikis.path().join("none")).contains("No wikis yet"));
    }

    #[test]
    fn a_wiki_is_stale_once_its_branch_has_moved_on() {
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(args)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .output()
                .unwrap()
        };
        if !git(&["init", "-q", "-b", "main"]).status.success() {
            return;
        }
        let commit = |message: &str| {
            git(&[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                message,
            ]);
            String::from_utf8(git(&["rev-parse", "HEAD"]).stdout)
                .unwrap()
                .trim()
                .to_string()
        };
        let first = commit("one");
        let root = dir.path().display().to_string();
        let wiki = json!({"repo": {"root": root, "commit": first, "branch": "main"}});
        assert!(!stale(&wiki));
        commit("two");
        assert!(stale(&wiki));
        assert!(
            !stale(&json!({"repo": {"root": root}})),
            "no commit, nothing to say"
        );
        let status = status(dir.path(), &wiki);
        assert_eq!(status["stale"], true);
        assert_eq!(status["building"], false);
        assert_eq!(status["progress"], Value::Null);
        fs::write(
            dir.path().join(BUILD_FILE),
            r#"{"building":true,"progress":"3/9 subsections"}"#,
        )
        .unwrap();
        // Written down by a build that was stopped: none is building.
        let status = super::status(dir.path(), &wiki);
        assert_eq!(status["building"], json!(false));
        let _held = wiki::book::lock(dir.path()).unwrap();
        let status = super::status(dir.path(), &wiki);
        assert_eq!(
            (status["building"].clone(), status["progress"].clone()),
            (json!(true), json!("3/9 subsections"))
        );
    }
}
