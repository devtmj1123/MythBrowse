// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! Transfer tests against a small local HTTP/1.1 server (std only).

use super::*;
use std::io::{BufRead, BufReader};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::AtomicBool;

#[derive(Debug, Clone, Default)]
struct Seen {
    path: String,
    host: String,
    headers: HashMap<String, String>,
}

struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    /// Pause between 4 KiB chunks.
    delay: Duration,
    /// Close the connection after this many body bytes (simulates a drop).
    cut_after: Option<usize>,
}

impl Reply {
    fn ok(body: &[u8]) -> Reply {
        Reply {
            status: 200,
            headers: vec![],
            body: body.to_vec(),
            delay: Duration::ZERO,
            cut_after: None,
        }
    }
    fn header(mut self, k: &str, v: &str) -> Reply {
        self.headers.push((k.into(), v.into()));
        self
    }
}

type Handler = Arc<dyn Fn(&Seen) -> Reply + Send + Sync>;

struct Server {
    port: u16,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Server {
    fn start(handler: impl Fn(&Seen) -> Reply + Send + Sync + 'static) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let handler: Handler = Arc::new(handler);
        let s2 = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let h = handler.clone();
                let s = s2.clone();
                std::thread::spawn(move || serve(stream, h, s));
            }
        });
        Server { port, seen }
    }
    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }
    fn url_other_host(&self, path: &str) -> String {
        format!("http://localhost:{}{path}", self.port)
    }
    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}

fn serve(stream: TcpStream, handler: Handler, seen: Arc<Mutex<Vec<Seen>>>) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }
    let path = line.split_whitespace().nth(1).unwrap_or("/").to_owned();
    let mut headers = HashMap::new();
    loop {
        let mut l = String::new();
        if reader.read_line(&mut l).is_err() || l.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = l.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_owned());
        }
    }
    let host = headers.get("host").cloned().unwrap_or_default();
    let req = Seen {
        path,
        host,
        headers,
    };
    seen.lock().unwrap().push(req.clone());
    let reply = handler(&req);
    let mut out = stream;
    let mut head = format!("HTTP/1.1 {} X\r\nConnection: close\r\n", reply.status);
    let has_len = reply
        .headers
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("content-length"));
    for (k, v) in &reply.headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    if !has_len {
        head.push_str(&format!("Content-Length: {}\r\n", reply.body.len()));
    }
    head.push_str("\r\n");
    if out.write_all(head.as_bytes()).is_err() {
        return;
    }
    let limit = reply
        .cut_after
        .unwrap_or(reply.body.len())
        .min(reply.body.len());
    for chunk in reply.body[..limit].chunks(4096) {
        if out.write_all(chunk).is_err() {
            return;
        }
        let _ = out.flush();
        if !reply.delay.is_zero() {
            std::thread::sleep(reply.delay);
        }
    }
    let _ = out.flush();
}

/// Serve `data` honouring `Range` + `If-Range` (validator `"v1"`).
fn ranged(data: Vec<u8>, req: &Seen, delay: Duration) -> Reply {
    let range = req
        .headers
        .get("range")
        .and_then(|r| r.strip_prefix("bytes="))
        .and_then(|r| r.trim_end_matches('-').parse::<usize>().ok());
    let if_range_ok = req.headers.get("if-range").is_none_or(|v| v == "\"v1\"");
    match range {
        Some(start) if if_range_ok && start < data.len() => Reply {
            status: 206,
            headers: vec![
                (
                    "Content-Range".into(),
                    format!("bytes {start}-{}/{}", data.len() - 1, data.len()),
                ),
                ("ETag".into(), "\"v1\"".into()),
                ("Accept-Ranges".into(), "bytes".into()),
                ("Content-Type".into(), "application/zip".into()),
            ],
            body: data[start..].to_vec(),
            delay,
            cut_after: None,
        },
        Some(start) if if_range_ok && start >= data.len() => Reply {
            status: 416,
            headers: vec![("Content-Range".into(), format!("bytes */{}", data.len()))],
            body: vec![],
            delay: Duration::ZERO,
            cut_after: None,
        },
        _ => Reply {
            status: 200,
            headers: vec![
                ("ETag".into(), "\"v1\"".into()),
                ("Accept-Ranges".into(), "bytes".into()),
                ("Content-Type".into(), "application/zip".into()),
            ],
            body: data,
            delay,
            cut_after: None,
        },
    }
}

struct Fixture {
    dir: PathBuf,
    mgr: Manager,
    events: Arc<Mutex<Vec<Event>>>,
}

impl Fixture {
    fn new(tag: &str) -> Fixture {
        static N: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "mb-dl-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let events = Arc::new(Mutex::new(Vec::new()));
        let ev = events.clone();
        let mgr = Manager::new(dir.clone(), 1, move |e| ev.lock().unwrap().push(e));
        Fixture { dir, mgr, events }
    }

    fn wait(&self, id: Id, pred: impl Fn(&Item) -> bool) -> Item {
        let start = Instant::now();
        loop {
            if let Some(i) = self.mgr.get(id)
                && pred(&i)
            {
                return i;
            }
            assert!(
                start.elapsed() < Duration::from_secs(20),
                "timeout; item = {:?}",
                self.mgr.get(id)
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn wait_state(&self, id: Id, state: State) -> Item {
        self.wait(id, |i| i.state == state)
    }

    fn files(&self) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(&self.dir)
            .map(|r| {
                r.flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        v.sort();
        v
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn req(url: String) -> Request {
    Request {
        url,
        ..Request::default()
    }
}

fn data(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i * 7 % 251) as u8).collect()
}

#[test]
fn downloads_with_content_disposition_name() {
    let s = Server::start(|_| {
        Reply::ok(b"hello world")
            .header(
                "Content-Disposition",
                "attachment; filename=\"report 2026.txt\"",
            )
            .header("Content-Type", "text/plain")
    });
    let f = Fixture::new("basic");
    let id = f.mgr.start(req(s.url("/get?id=1")));
    let item = f.wait_state(id, State::Done);
    assert_eq!(item.name, "report 2026.txt");
    assert_eq!(item.received, 11);
    assert_eq!(item.total, Some(11));
    assert_eq!(std::fs::read(&item.path).unwrap(), b"hello world");
    assert_eq!(f.files(), vec!["report 2026.txt".to_string()]);
    assert!(item.finished_ms >= item.started_ms);
    // A second one gets a fresh name.
    let id2 = f.mgr.start(req(s.url("/get?id=2")));
    assert_eq!(f.wait_state(id2, State::Done).name, "report 2026 (1).txt");
    assert!(
        f.events
            .lock()
            .unwrap()
            .iter()
            .any(|e| matches!(e, Event::Changed(i) if i.state == State::Done))
    );
}

#[test]
fn pause_and_resume_with_range() {
    let body = data(600 * 1024);
    let b2 = body.clone();
    let s = Server::start(move |r| ranged(b2.clone(), r, Duration::from_millis(5)));
    let f = Fixture::new("pause");
    let id = f.mgr.start(req(s.url("/big.zip")));
    f.wait(id, |i| i.state == State::Running && i.received > 64 * 1024);
    f.mgr.pause(id).unwrap();
    let paused = f.wait_state(id, State::Paused);
    assert!(paused.resumable, "{paused:?}");
    std::thread::sleep(Duration::from_millis(100));
    let part = names::part_path(Path::new(&paused.path));
    let have = std::fs::metadata(&part).unwrap().len();
    assert!(have > 0 && have < body.len() as u64);
    f.mgr.resume(id).unwrap();
    let done = f.wait_state(id, State::Done);
    assert_eq!(std::fs::read(&done.path).unwrap(), body);
    assert!(!part.exists());
    let seen = s.seen();
    let resumed = seen
        .iter()
        .find(|r| r.headers.contains_key("range"))
        .expect("a ranged request");
    assert_eq!(
        resumed.headers.get("if-range").map(String::as_str),
        Some("\"v1\"")
    );
    assert!(resumed.headers["range"].starts_with("bytes="));
}

#[test]
fn resume_restarts_when_server_ignores_range() {
    let body = data(300 * 1024);
    let b2 = body.clone();
    let s = Server::start(move |_| {
        // Advertises ranges but always answers 200 with the whole file.
        Reply {
            delay: Duration::from_millis(5),
            ..Reply::ok(&b2)
        }
        .header("Accept-Ranges", "bytes")
        .header("ETag", "\"v1\"")
        .header("Content-Type", "application/zip")
    });
    let f = Fixture::new("norange");
    let id = f.mgr.start(req(s.url("/a.zip")));
    f.wait(id, |i| i.received > 32 * 1024);
    f.mgr.pause(id).unwrap();
    f.wait_state(id, State::Paused);
    std::thread::sleep(Duration::from_millis(50));
    f.mgr.resume(id).unwrap();
    let done = f.wait_state(id, State::Done);
    assert_eq!(std::fs::read(&done.path).unwrap(), body);
}

#[test]
fn already_complete_part_finishes_on_416() {
    let body = data(10_000);
    let b2 = body.clone();
    let s = Server::start(move |r| ranged(b2.clone(), r, Duration::ZERO));
    let f = Fixture::new("416");
    let id = f.mgr.start(req(s.url("/x.zip")));
    let done = f.wait_state(id, State::Done);
    // Pretend we were interrupted after the last byte: put the file back as .part.
    let part = names::part_path(Path::new(&done.path));
    std::fs::rename(&done.path, &part).unwrap();
    f.mgr.restore(vec![Item {
        state: State::Running,
        ..done.clone()
    }]);
    assert_eq!(f.mgr.get(id).unwrap().state, State::Paused);
    f.mgr.resume(id).unwrap();
    let again = f.wait_state(id, State::Done);
    assert_eq!(std::fs::read(&again.path).unwrap(), body);
    assert!(
        s.seen()
            .iter()
            .any(|r| r.headers.get("range").is_some_and(|v| v == "bytes=10000-"))
    );
}

#[test]
fn dropped_connection_fails_then_retry_resumes() {
    let body = data(200 * 1024);
    let b2 = body.clone();
    let first = Arc::new(AtomicBool::new(true));
    let s = Server::start(move |r| {
        let mut rep = ranged(b2.clone(), r, Duration::ZERO);
        if first.swap(false, Ordering::SeqCst) {
            rep.cut_after = Some(50 * 1024);
        }
        rep
    });
    let f = Fixture::new("drop");
    let id = f.mgr.start(req(s.url("/drop.zip")));
    let failed = f.wait_state(id, State::Failed);
    assert!(failed.error.contains("Retry"), "{}", failed.error);
    f.mgr.retry(id).unwrap();
    let done = f.wait_state(id, State::Done);
    assert_eq!(std::fs::read(&done.path).unwrap(), body);
    assert!(s.seen().iter().any(|r| r.headers.contains_key("range")));
}

#[test]
fn dangerous_type_waits_for_keep_and_writes_nothing_before() {
    let s = Server::start(|_| {
        Reply::ok(b"MZ fake program").header("Content-Type", "application/x-msdownload")
    });
    let f = Fixture::new("danger");
    let id = f.mgr.start(req(s.url("/setup.exe")));
    let c = f.wait_state(id, State::Confirm);
    assert!(c.danger);
    assert!(!c.warning().is_empty());
    assert!(f.mgr.needs_attention());
    assert!(
        f.files().is_empty(),
        "nothing on disk before Keep: {:?}",
        f.files()
    );
    f.mgr.keep(id).unwrap();
    let done = f.wait_state(id, State::Done);
    assert_eq!(std::fs::read(&done.path).unwrap(), b"MZ fake program");

    let id2 = f.mgr.start(req(s.url("/other.exe")));
    f.wait_state(id2, State::Confirm);
    f.mgr.discard(id2).unwrap();
    f.wait_state(id2, State::Cancelled);
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(f.files(), vec!["setup.exe".to_string()]);
    assert!(!f.mgr.needs_attention());
}

#[test]
fn cancel_deletes_part() {
    let body = data(800 * 1024);
    let s = Server::start(move |r| ranged(body.clone(), r, Duration::from_millis(5)));
    let f = Fixture::new("cancel");
    let id = f.mgr.start(req(s.url("/c.zip")));
    let running = f.wait(id, |i| i.received > 16 * 1024);
    f.mgr.cancel(id).unwrap();
    f.wait_state(id, State::Cancelled);
    std::thread::sleep(Duration::from_millis(100));
    assert!(!names::part_path(Path::new(&running.path)).exists());
    assert!(f.files().is_empty(), "{:?}", f.files());
    // Cancelled can be retried from scratch.
    f.mgr.retry(id).unwrap();
    f.wait_state(id, State::Done);
}

#[test]
fn cookies_only_to_original_host_and_referrer_policy() {
    let s = Server::start(|r| {
        if r.path == "/start" {
            let port = r.host.rsplit(':').next().unwrap_or("0").to_owned();
            Reply {
                status: 302,
                ..Reply::ok(b"")
            }
            .header("Location", &format!("http://localhost:{port}/file.zip"))
        } else {
            Reply::ok(b"zipzip").header("Content-Type", "application/zip")
        }
    });
    let f = Fixture::new("cookies");
    let mut r = req(s.url("/start"));
    r.cookies = Some("sid=secret".into());
    r.referrer = Some(s.url("/page.html#frag"));
    let id = f.mgr.start(r);
    let done = f.wait_state(id, State::Done);
    assert_eq!(done.name, "file.zip");
    assert!(done.final_url.contains("localhost"));
    let seen = s.seen();
    assert_eq!(seen.len(), 2);
    assert_eq!(
        seen[0].headers.get("cookie").map(String::as_str),
        Some("sid=secret")
    );
    assert_eq!(
        seen[0].headers.get("referer").cloned(),
        Some(s.url("/page.html"))
    );
    assert!(
        !seen[1].headers.contains_key("cookie"),
        "cookie leaked to another host"
    );
    assert_eq!(
        seen[1].headers.get("referer").cloned(),
        Some(format!("http://127.0.0.1:{}/", s.port))
    );
    let _ = s.url_other_host("/");
}

#[test]
fn html_answer_to_a_sniffed_navigation_is_not_a_download() {
    let s = Server::start(|_| Reply::ok(b"<html>login</html>").header("Content-Type", "text/html"));
    let f = Fixture::new("sniff");
    let mut r = req(s.url("/report.pdf"));
    r.sniff_html = true;
    let id = f.mgr.start(r);
    let start = Instant::now();
    while f.mgr.get(id).is_some() {
        assert!(start.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        f.events
            .lock()
            .unwrap()
            .iter()
            .any(|e| matches!(e, Event::NotADownload { id: x, .. } if *x == id))
    );
    assert!(f.files().is_empty());
    // An attachment is still a download even if it says text/html.
    let s2 = Server::start(|_| {
        Reply::ok(b"<p>x</p>")
            .header("Content-Type", "text/html")
            .header("Content-Disposition", "attachment; filename=page.html")
    });
    let mut r = req(s2.url("/x.pdf"));
    r.sniff_html = true;
    let id = f.mgr.start(r);
    assert_eq!(f.wait_state(id, State::Done).name, "page.html");
}

#[test]
fn http_errors_are_plain_words() {
    let s = Server::start(|_| Reply {
        status: 404,
        ..Reply::ok(b"nope")
    });
    let f = Fixture::new("404");
    let id = f.mgr.start(req(s.url("/missing.zip")));
    let failed = f.wait_state(id, State::Failed);
    assert!(
        failed.error.contains("not on the server"),
        "{}",
        failed.error
    );
    assert!(f.files().is_empty());
}

#[test]
fn data_and_blob_urls() {
    let f = Fixture::new("data");
    let mut r = req("data:text/plain;base64,aGVsbG8=".into());
    r.suggested_name = Some("hi.txt".into());
    let id = f.mgr.start(r);
    let done = f.wait_state(id, State::Done);
    assert_eq!(done.name, "hi.txt");
    assert_eq!(std::fs::read(&done.path).unwrap(), b"hello");
    let id = f.mgr.start(req("data:,a%20b".into()));
    let done = f.wait_state(id, State::Done);
    assert_eq!(std::fs::read(&done.path).unwrap(), b"a b");
    assert!(done.name.ends_with(".txt"), "{}", done.name);
    let id = f.mgr.start(req("blob:https://x.test/1234".into()));
    assert!(f.wait_state(id, State::Failed).error.contains("blob"));
}

#[test]
fn list_progress_clear_and_remove() {
    let s = Server::start(|_| Reply::ok(b"abc").header("Content-Type", "application/zip"));
    let f = Fixture::new("list");
    let a = f.mgr.start(req(s.url("/a.zip")));
    let b = f.mgr.start(req(s.url("/b.zip")));
    f.wait_state(a, State::Done);
    f.wait_state(b, State::Done);
    let l = f.mgr.list();
    assert_eq!(l.iter().map(|i| i.id).collect::<Vec<_>>(), vec![b, a]);
    assert_eq!(f.mgr.progress().0, 0);
    assert!(f.mgr.remove(a).is_some());
    assert!(Path::new(&l[1].path).exists(), "remove keeps the file");
    assert_eq!(f.mgr.clear_finished(), vec![b]);
    assert!(f.mgr.list().is_empty());
}

#[test]
fn helpers() {
    assert_eq!(
        parse_content_range("bytes 100-199/1000"),
        Some((100, 199, Some(1000)))
    );
    assert_eq!(parse_content_range("bytes 0-0/*"), Some((0, 0, None)));
    assert_eq!(parse_content_range("items 1-2/3"), None);
    let r = Url::parse("https://a.test/p?q=1#f").unwrap();
    assert_eq!(
        referrer_for(&r, &Url::parse("https://a.test/x").unwrap()).as_deref(),
        Some("https://a.test/p?q=1")
    );
    assert_eq!(
        referrer_for(&r, &Url::parse("https://b.test/x").unwrap()).as_deref(),
        Some("https://a.test/")
    );
    assert_eq!(
        referrer_for(&r, &Url::parse("http://a.test/x").unwrap()),
        None
    );
    assert_eq!(
        decode_data_url("data:image/png;base64,iVBORw==").unwrap().0,
        "image/png"
    );
    assert_eq!(
        decode_data_url("data:;base64,YWJj").unwrap(),
        ("text/plain".into(), b"abc".to_vec())
    );
    assert!(decode_data_url("data:nocomma").is_none());
    assert_eq!(State::parse(State::Confirm.as_str()), Some(State::Confirm));
    assert_eq!(
        serde_json::to_string(&State::Running).unwrap(),
        "\"running\""
    );
}

#[test]
fn the_same_file_is_fetched_once_from_several_hooks() {
    let s = Server::start(|_| {
        let mut r = Reply::ok(&data(64 * 1024)).header("Content-Type", "application/zip");
        r.delay = Duration::from_millis(5);
        r
    });
    let f = Fixture::new("once");
    let url = s.url("/files/report.zip");
    // The navigation hook, then the main-frame hook / unknown-content page for the same URL.
    let first = f.mgr.start_once(req(url.clone()), Duration::from_secs(10));
    assert!(first.is_some());
    assert!(
        f.mgr
            .start_once(req(url.clone()), Duration::from_secs(10))
            .is_none()
    );
    let done = f.wait_state(first.unwrap(), State::Done);
    // Finished, but still within the window: not fetched again.
    assert!(
        f.mgr
            .start_once(req(url.clone()), Duration::from_secs(10))
            .is_none()
    );
    assert_eq!(
        s.seen()
            .iter()
            .filter(|r| r.path == "/files/report.zip")
            .count(),
        1,
        "one request for the file"
    );
    assert_eq!(done.received, 64 * 1024);
    // Outside the window (a deliberate second download later) it is fetched again.
    let again = f.mgr.start_once(req(url), Duration::ZERO);
    assert!(again.is_some());
    f.wait_state(again.unwrap(), State::Done);
    assert_eq!(s.seen().len(), 2);
}

#[test]
fn ask_where_to_save() {
    let s = Server::start(|_| {
        Reply::ok(b"asked").header("Content-Disposition", "attachment; filename=\"a.txt\"")
    });
    let f = Fixture::new("ask");
    let elsewhere = f.dir.join("chosen");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let asked = Arc::new(Mutex::new(Vec::new()));
    let (a2, e2) = (asked.clone(), elsewhere.clone());
    f.mgr.set_ask(Some(Arc::new(move |dir: &Path, name: &str| {
        a2.lock()
            .unwrap()
            .push((dir.to_path_buf(), name.to_owned()));
        Some(e2.join("renamed.txt"))
    })));
    assert!(f.mgr.asks());
    let id = f.mgr.start(req(s.url("/a")));
    let item = f.wait_state(id, State::Done);
    assert_eq!(item.name, "renamed.txt");
    assert_eq!(
        std::fs::read(elsewhere.join("renamed.txt")).unwrap(),
        b"asked"
    );
    assert_eq!(asked.lock().unwrap().len(), 1);
    assert_eq!(asked.lock().unwrap()[0].1, "a.txt");

    // Cancelled: the download fails with NOT_SAVED and nothing is written.
    f.mgr.set_ask(Some(Arc::new(|_: &Path, _: &str| None)));
    let id = f.mgr.start(req(s.url("/b")));
    let item = f.wait_state(id, State::Failed);
    assert_eq!(item.error, NOT_SAVED);
    // Back to saving straight into the folder.
    f.mgr.set_ask(None);
    assert!(!f.mgr.asks());
    let id = f.mgr.start(req(s.url("/c")));
    assert_eq!(f.wait_state(id, State::Done).name, "a.txt");
}
