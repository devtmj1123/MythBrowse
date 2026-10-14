// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! `mb-rules`: compat ruleset tool.
//!
//! ```text
//! mb-rules check   <ruleset.toml> [builtin|remote|local]   validate against schema + trust rules
//! mb-rules scripts <ruleset.toml> <out-dir>                write the generated user scripts
//!                                                           (for `node --check`)
//! mb-rules keygen  <private-key-file>                       new Ed25519 release key; prints the
//!                                                           public half for sign::TRUSTED_KEYS
//! mb-rules sign    <private-key-file> <ruleset.toml>        writes <ruleset.toml>.sig
//! mb-rules verify  <ruleset.toml> <public-key-hex>          checks <ruleset.toml>.sig
//! ```
//!
//! Keep the private key file offline (or in the publishing job's secret store), never in git.

use std::process::ExitCode;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use mythbrowse_compat::rules::{RulesetFile, Source};
use mythbrowse_compat::{Compat, sign};

const KEY_HEADER: &str = "mythbrowse-compat-ed25519-pkcs8";

fn usage() -> ExitCode {
    eprintln!(
        "usage:\n  mb-rules check <ruleset.toml> [builtin|remote|local]\n  mb-rules scripts <ruleset.toml> <out-dir>\n  mb-rules keygen <private-key-file>\n  mb-rules sign <private-key-file> <ruleset.toml>\n  mb-rules verify <ruleset.toml> <public-key-hex>"
    );
    ExitCode::from(2)
}

fn fail(msg: impl std::fmt::Display) -> ExitCode {
    eprintln!("mb-rules: {msg}");
    ExitCode::FAILURE
}

fn read(path: &str) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    match a.as_slice() {
        ["check", file, rest @ ..] => {
            let source = match rest.first().copied().unwrap_or("builtin") {
                "builtin" => Source::Builtin,
                "remote" => Source::Remote,
                "local" => Source::Local,
                _ => return usage(),
            };
            let text = match read(file) {
                Ok(t) => t,
                Err(e) => return fail(e),
            };
            match RulesetFile::parse(&text, source) {
                Ok(f) => {
                    println!(
                        "{file}: ruleset `{}` v{} OK, {} rule(s)",
                        f.id,
                        f.version,
                        f.rules.len()
                    );
                    for r in &f.rules {
                        println!("  {} ({:?}) hosts={:?}", r.id, r.kind, r.hosts);
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => fail(format!("{file}: {e}")),
            }
        }
        ["scripts", file, out] => {
            let text = match read(file) {
                Ok(t) => t,
                Err(e) => return fail(e),
            };
            let f = match RulesetFile::parse(&text, Source::Builtin) {
                Ok(f) => f,
                Err(e) => return fail(format!("{file}: {e}")),
            };
            // Build the bundle the browser would install with only this ruleset's rules.
            let (compat, _) = Compat::load(None, mythbrowse_compat::session_key(0));
            let mut scripts = vec![compat.user_scripts().remove(0)];
            scripts.extend(mythbrowse_compat::bundle_for_check(&f));
            if let Err(e) = std::fs::create_dir_all(out) {
                return fail(format!("{out}: {e}"));
            }
            for (i, s) in scripts.iter().enumerate() {
                let path = std::path::Path::new(out).join(format!("{i:03}.js"));
                if let Err(e) = std::fs::write(&path, s) {
                    return fail(format!("{}: {e}", path.display()));
                }
            }
            let probe = std::path::Path::new(out).join("probe.js");
            if let Err(e) = std::fs::write(&probe, compat.probe_script()) {
                return fail(format!("{}: {e}", probe.display()));
            }
            println!("wrote {} script(s) + probe.js to {out}", scripts.len());
            ExitCode::SUCCESS
        }
        ["keygen", key_file] => {
            if std::path::Path::new(key_file).exists() {
                return fail(format!("{key_file} exists; refusing to overwrite a key"));
            }
            let (private, public) = match sign::generate() {
                Ok(k) => k,
                Err(e) => return fail(e),
            };
            let body = format!("{KEY_HEADER}\n{}\n", B64.encode(&private));
            if let Err(e) = std::fs::write(key_file, body) {
                return fail(format!("{key_file}: {e}"));
            }
            let id = sign::key_id(&public);
            println!("private key written to {key_file} (keep it offline, never commit it)");
            println!("public key (hex): {}", hex(&public));
            println!("add to crates/compat/src/sign.rs TRUSTED_KEYS:");
            let bytes: Vec<String> = public.iter().map(|b| format!("0x{b:02x}")).collect();
            println!(
                "    TrustedKey {{ id: \"{id}\", public: [{}] }},",
                bytes.join(", ")
            );
            ExitCode::SUCCESS
        }
        ["sign", key_file, file] => {
            let key = match read(key_file) {
                Ok(t) => t,
                Err(e) => return fail(e),
            };
            let mut lines = key.lines();
            if lines.next() != Some(KEY_HEADER) {
                return fail(format!("{key_file}: not a mb-rules private key"));
            }
            let Some(pkcs8) = lines.next().and_then(|l| B64.decode(l.trim()).ok()) else {
                return fail(format!("{key_file}: unreadable key"));
            };
            let text = match read(file) {
                Ok(t) => t,
                Err(e) => return fail(e),
            };
            if let Err(e) = RulesetFile::parse(&text, Source::Remote) {
                return fail(format!("{file}: {e}"));
            }
            let sig = match sign::sign(&pkcs8, text.as_bytes()) {
                Ok(s) => s,
                Err(e) => return fail(e),
            };
            let out = format!("{file}.sig");
            if let Err(e) = std::fs::write(&out, sig) {
                return fail(format!("{out}: {e}"));
            }
            println!("signed {file} -> {out}");
            ExitCode::SUCCESS
        }
        ["verify", file, public_hex] => {
            let Some(public) = unhex(public_hex).filter(|p| p.len() == 32) else {
                return fail("public key must be 64 hex characters");
            };
            let (text, sig) = match (read(file), read(&format!("{file}.sig"))) {
                (Ok(t), Ok(s)) => (t, s),
                (Err(e), _) | (_, Err(e)) => return fail(e),
            };
            let id: &'static str = Box::leak(sign::key_id(&public).into_boxed_str());
            let key = sign::TrustedKey {
                id,
                public: public.as_slice().try_into().expect("32 bytes"),
            };
            match sign::verify(text.as_bytes(), &sig, &[key]) {
                Ok(id) => {
                    println!("{file}: signature OK (key {id})");
                    ExitCode::SUCCESS
                }
                Err(e) => fail(format!("{file}: {e}")),
            }
        }
        _ => usage(),
    }
}
