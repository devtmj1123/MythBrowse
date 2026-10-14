// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

use super::*;
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A tiny HTTP/1.1 server on 127.0.0.1: path -> (status, body). Counts requests per path.
struct Server {
    base: String,
    hits: Arc<Mutex<HashMap<String, usize>>>,
}

fn sha_hex(b: &[u8]) -> String {
    Sha256::digest(b)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}

fn release_json(base: &str, tag: &str, size: u64, with_sum: bool) -> Vec<u8> {
    let mut assets = vec![serde_json::json!({
        "name": "MythBrowse-x64.msi", "size": size,
        "browser_download_url": format!("{base}/dl/MythBrowse-x64.msi"),
    })];
    if with_sum {
        assets.push(serde_json::json!({
            "name": "MythBrowse-x64.msi.sha256", "size": 64,
            "browser_download_url": format!("{base}/dl/MythBrowse-x64.msi.sha256"),
        }));
    }
    serde_json::json!({
        "tag_name": tag, "name": "MythBrowse", "html_url": format!("{base}/rel"),
        "draft": false, "prerelease": false, "assets": assets,
    })
    .to_string()
    .into_bytes()
}

struct Fixture {
    up: Updater,
    launched: Arc<Mutex<Vec<PathBuf>>>,
    changes: Arc<AtomicUsize>,
    dir: PathBuf,
}

fn fixture(base: &str, current: &str, tag: &str) -> Fixture {
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "mb-update-test-{}-{}-{}",
        std::process::id(),
        tag,
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let launched = Arc::new(Mutex::new(Vec::new()));
    let l = launched.clone();
    let mut cfg = Config::new("devtmj1123/MythBrowse", current);
    cfg.api_base = base.to_owned();
    cfg.state_file = Some(dir.join("update.json"));
    cfg.download_dir = dir.join("dl");
    let b = base.to_owned();
    cfg.allow_url = Arc::new(move |u| u.starts_with(&b));
    cfg.launcher = Arc::new(move |p| {
        l.lock().unwrap().push(p.to_path_buf());
        Ok(())
    });
    cfg.arch = Arch::X64;
    // The fixtures publish official-style names (`MythBrowse-x64.msi`).
    cfg.channel = Channel::Official;
    let changes = Arc::new(AtomicUsize::new(0));
    let c = changes.clone();
    let up = Updater::new(cfg, move || {
        c.fetch_add(1, Ordering::SeqCst);
    });
    Fixture {
        up,
        launched,
        changes,
        dir,
    }
}

#[test]
fn once_a_day() {
    let now = 1_800_000_000_000;
    assert!(due(None, now));
    assert!(!due(Some(now - 1000), now));
    assert!(!due(Some(now - CHECK_INTERVAL_MS + 1), now));
    assert!(due(Some(now - CHECK_INTERVAL_MS), now));
    assert!(
        due(Some(now + 60_000), now),
        "a clock that went back counts as due"
    );
}

#[test]
fn up_to_date_and_remembers_the_check() {
    let s = serve_self(|base| {
        vec![(
            "/repos/devtmj1123/MythBrowse/releases/latest".to_owned(),
            200,
            release_json(base, "v0.4.0", 3, false),
        )]
    });
    let f = fixture(&s.base, "0.4.0", "same");
    assert!(f.up.due());
    f.up.run_check();
    let st = f.up.status();
    assert_eq!(st.phase, Phase::UpToDate);
    assert_eq!(st.latest.as_deref(), Some("0.4.0"));
    assert!(st.checked_ms.is_some());
    assert!(f.changes.load(Ordering::SeqCst) > 0);
    // The check time survives a restart: the next automatic check waits a day.
    let reread = Updater::new(
        Config {
            state_file: Some(f.dir.join("update.json")),
            ..Config::new("devtmj1123/MythBrowse", "0.4.0")
        },
        || {},
    );
    assert!(!reread.due());
    assert!(!reread.check(false), "not due: no automatic check");
    let _ = std::fs::remove_dir_all(&f.dir);
}

#[test]
fn no_release_yet_is_up_to_date() {
    let s = serve_self(|_| vec![]);
    let f = fixture(&s.base, "0.0.1", "none");
    f.up.run_check();
    assert_eq!(f.up.status().phase, Phase::UpToDate);
    assert_eq!(f.up.status().latest, None);
}

#[test]
fn rate_limited_is_an_error() {
    let s = serve_self(|_| {
        vec![(
            "/repos/devtmj1123/MythBrowse/releases/latest".to_owned(),
            403,
            b"{}".to_vec(),
        )]
    });
    let f = fixture(&s.base, "0.0.1", "403");
    f.up.run_check();
    let st = f.up.status();
    assert_eq!(st.phase, Phase::Error);
    assert!(st.error.unwrap().contains("limiting"));
}

#[test]
fn downloads_verifies_and_launches() {
    let msi = b"MSI-BYTES-0123456789".to_vec();
    let sum = format!("{}  MythBrowse-x64.msi\n", sha_hex(&msi)).into_bytes();
    let s = serve_self(|base| {
        vec![
            (
                "/repos/devtmj1123/MythBrowse/releases/latest".to_owned(),
                200,
                release_json(base, "v0.5.0", msi.len() as u64, true),
            ),
            ("/dl/MythBrowse-x64.msi".to_owned(), 200, msi.clone()),
            ("/dl/MythBrowse-x64.msi.sha256".to_owned(), 200, sum.clone()),
        ]
    });
    let f = fixture(&s.base, "0.4.0", "ok");
    f.up.run_check();
    let st = f.up.status();
    assert_eq!(st.phase, Phase::Available, "{st:?}");
    assert_eq!(st.latest.as_deref(), Some("0.5.0"));
    assert_eq!(st.installer.as_deref(), Some("MythBrowse-x64.msi"));
    assert!(st.checksum);
    f.up.lock().busy = true;
    assert!(f.up.install().is_err(), "busy: no second install");
    f.up.lock().busy = false;
    f.up.run_install();
    let st = f.up.status();
    assert_eq!(st.phase, Phase::Installing, "{st:?}");
    assert_eq!(st.received, msi.len() as u64);
    let launched = f.launched.lock().unwrap().clone();
    assert_eq!(launched.len(), 1);
    assert_eq!(std::fs::read(&launched[0]).unwrap(), msi);
    assert!(!f.dir.join("dl/MythBrowse-x64.msi.part").exists());
    let _ = std::fs::remove_dir_all(&f.dir);
}

#[test]
fn wrong_size_or_hash_is_discarded() {
    let msi = b"MSI-BYTES".to_vec();
    for (case, size, sum) in [
        ("short", msi.len() as u64 + 5, None),
        ("long", msi.len() as u64 - 2, None),
        (
            "hash",
            msi.len() as u64,
            Some(format!("{}  MythBrowse-x64.msi", "0".repeat(64))),
        ),
        ("nosum", msi.len() as u64, Some("garbage".to_owned())),
    ] {
        let s = serve_self(|base| {
            let mut r = vec![
                (
                    "/repos/devtmj1123/MythBrowse/releases/latest".to_owned(),
                    200,
                    release_json(base, "v9.0.0", size, sum.is_some()),
                ),
                ("/dl/MythBrowse-x64.msi".to_owned(), 200, msi.clone()),
            ];
            if let Some(t) = &sum {
                r.push((
                    "/dl/MythBrowse-x64.msi.sha256".to_owned(),
                    200,
                    t.clone().into_bytes(),
                ));
            }
            r
        });
        let f = fixture(&s.base, "1.0.0", case);
        f.up.run_check();
        assert_eq!(f.up.status().phase, Phase::Available, "{case}");
        f.up.run_install();
        let st = f.up.status();
        assert_eq!(st.phase, Phase::Error, "{case}: {st:?}");
        assert!(
            f.launched.lock().unwrap().is_empty(),
            "{case}: never launched"
        );
        assert!(!f.dir.join("dl/MythBrowse-x64.msi").exists(), "{case}");
        assert!(!f.dir.join("dl/MythBrowse-x64.msi.part").exists(), "{case}");
        if case == "nosum" {
            // The checksum is read first: the installer itself was never fetched.
            assert_eq!(s.hits.lock().unwrap().get("/dl/MythBrowse-x64.msi"), None);
        }
        let _ = std::fs::remove_dir_all(&f.dir);
    }
}

#[test]
fn untrusted_download_url_is_never_offered() {
    let s = serve_self(|_| {
        vec![(
            "/repos/devtmj1123/MythBrowse/releases/latest".to_owned(),
            200,
            release_json("https://evil.example", "v9.0.0", 5, false),
        )]
    });
    let f = fixture(&s.base, "1.0.0", "evil");
    f.up.run_check();
    assert_eq!(f.up.status().phase, Phase::NoInstaller);
    assert!(f.up.install().is_err());
}

#[test]
fn default_config_trusts_only_the_public_repo() {
    let cfg = Config::new(crate::RELEASE_REPO, "0.0.1");
    assert!((cfg.allow_url)(
        "https://github.com/devtmj1123/MythBrowse/releases/download/v1/MythBrowse-x64.msi"
    ));
    assert!(!(cfg.allow_url)(
        "https://github.com/devtmj1123/MythBrowse-fork/releases/download/v1/a.msi"
    ));
    assert_eq!(cfg.api_base, "https://api.github.com");
    assert_eq!(cfg.channel, Channel::current());
}

#[test]
fn open_build_ignores_official_installers() {
    let s = serve_self(|base| {
        vec![(
            "/repos/devtmj1123/MythBrowse/releases/latest".to_owned(),
            200,
            release_json(base, "v9.0.0", 10, true),
        )]
    });
    let mut f = fixture(&s.base, "0.4.0", "open");
    f.up.cfg.channel = Channel::Open;
    f.up.run_check();
    assert_eq!(f.up.status().phase, Phase::NoInstaller);
}

/// [`serve`] whose routes can name the server's own base URL.
fn serve_self(f: impl FnOnce(&str) -> Vec<(String, u16, Vec<u8>)>) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let routes: HashMap<String, (u16, Vec<u8>)> =
        f(&base).into_iter().map(|(p, s, b)| (p, (s, b))).collect();
    let hits = Arc::new(Mutex::new(HashMap::new()));
    let h = hits.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            if reader.read_line(&mut line).is_err() {
                continue;
            }
            let path = line.split_whitespace().nth(1).unwrap_or("/").to_owned();
            loop {
                let mut l = String::new();
                if reader.read_line(&mut l).unwrap_or(0) == 0 || l == "\r\n" {
                    break;
                }
            }
            *h.lock().unwrap().entry(path.clone()).or_insert(0) += 1;
            let (status, body) = routes
                .get(&path)
                .cloned()
                .unwrap_or((404, b"nope".to_vec()));
            let head = format!(
                "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&body);
        }
    });
    Server { base, hits }
}
