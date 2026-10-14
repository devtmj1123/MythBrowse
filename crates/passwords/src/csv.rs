// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! Password CSV files (RFC 4180).
//!
//! Import understands the exports of Chrome / Edge (`name,url,username,password,note`),
//! Firefox (`url,username,password,httpRealm,formActionOrigin,...`), Safari
//! (`Title,URL,Username,Password,Notes,OTPAuth`) and Bitwarden (`login_uri,login_username,
//! login_password,...`): columns are found by header name, case-insensitively. Export writes
//! the Chrome format, so the file opens in Chrome, Edge, Firefox and most managers.

use mythbrowse_ai::secrets::Secret;
use zeroize::Zeroizing;

/// One login read from a CSV file.
#[derive(Debug, Clone)]
pub struct CsvLogin {
    pub name: String,
    pub url: String,
    pub username: String,
    pub password: Secret,
    pub note: String,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CsvError {
    #[error("the file is empty")]
    Empty,
    #[error("no URL and password columns found in the header line")]
    NoColumns,
    #[error("unterminated quoted field")]
    Unterminated,
}

/// Split CSV text into records. Handles quotes, doubled quotes, CRLF / LF and newlines
/// inside quoted fields; a UTF-8 BOM is ignored. Field strings are zeroed when dropped.
pub fn parse_records(text: &str) -> Result<Vec<Vec<Zeroizing<String>>>, CsvError> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut records = Vec::new();
    let mut record: Vec<Zeroizing<String>> = Vec::new();
    let mut field = Zeroizing::new(String::new());
    let mut chars = text.chars().peekable();
    let mut in_quotes = false;
    let mut any = false;
    while let Some(c) = chars.next() {
        any = true;
        if in_quotes {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    chars.next();
                    field.push('"');
                }
                '"' => in_quotes = false,
                _ => field.push(c),
            }
            continue;
        }
        match c {
            '"' => in_quotes = true,
            ',' => record.push(std::mem::replace(&mut field, Zeroizing::new(String::new()))),
            '\r' if chars.peek() == Some(&'\n') => {}
            '\n' | '\r' => {
                record.push(std::mem::replace(&mut field, Zeroizing::new(String::new())));
                if !(record.len() == 1 && record[0].is_empty()) {
                    records.push(std::mem::take(&mut record));
                } else {
                    record.clear();
                }
            }
            _ => field.push(c),
        }
    }
    if in_quotes {
        return Err(CsvError::Unterminated);
    }
    if any && (!field.is_empty() || !record.is_empty()) {
        record.push(field);
        records.push(record);
    }
    Ok(records)
}

fn find(header: &[String], names: &[&str]) -> Option<usize> {
    names
        .iter()
        .find_map(|n| header.iter().position(|h| h == n))
}

/// Read logins from a password CSV. Rows without a URL or password are skipped and counted.
pub fn parse_logins(text: &str) -> Result<(Vec<CsvLogin>, usize), CsvError> {
    let records = parse_records(text)?;
    let mut it = records.into_iter();
    let header: Vec<String> = it
        .next()
        .ok_or(CsvError::Empty)?
        .iter()
        .map(|h| h.trim().to_ascii_lowercase())
        .collect();
    let url = find(
        &header,
        &["url", "login_uri", "website", "origin", "web site"],
    );
    let user = find(
        &header,
        &[
            "username",
            "login_username",
            "user",
            "login",
            "email",
            "user name",
        ],
    );
    let pass = find(&header, &["password", "login_password", "pass"]);
    let name = find(&header, &["name", "title"]);
    let note = find(&header, &["note", "notes", "extra", "comments"]);
    let (Some(url), Some(pass)) = (url, pass) else {
        return Err(CsvError::NoColumns);
    };
    let mut out = Vec::new();
    let mut skipped = 0;
    for rec in it {
        let get = |i: Option<usize>| i.and_then(|i| rec.get(i)).map(|s| s.as_str()).unwrap_or("");
        let (u, p) = (get(Some(url)).trim(), get(Some(pass)));
        if u.is_empty() || p.is_empty() {
            skipped += 1;
            continue;
        }
        out.push(CsvLogin {
            name: get(name).to_owned(),
            url: u.to_owned(),
            username: get(user).to_owned(),
            password: Secret::new(p),
            note: get(note).to_owned(),
        });
    }
    Ok((out, skipped))
}

fn push_field(out: &mut String, s: &str) {
    if s.contains([',', '"', '\n', '\r']) || s.starts_with(' ') || s.ends_with(' ') {
        out.push('"');
        out.push_str(&s.replace('"', "\"\""));
        out.push('"');
    } else {
        out.push_str(s);
    }
}

/// Write logins in Chrome's CSV format (`name,url,username,password,note`, CRLF lines).
pub fn write_logins<'a>(rows: impl IntoIterator<Item = &'a CsvLogin>) -> Zeroizing<String> {
    let mut out = Zeroizing::new(String::from("name,url,username,password,note\r\n"));
    for r in rows {
        for (i, f) in [
            r.name.as_str(),
            r.url.as_str(),
            r.username.as_str(),
            r.password.expose(),
            r.note.as_str(),
        ]
        .iter()
        .enumerate()
        {
            if i > 0 {
                out.push(',');
            }
            push_field(&mut out, f);
        }
        out.push_str("\r\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chrome_csv_with_quotes_and_newlines() {
        let text = "\u{feff}name,url,username,password,note\r\n\
            example.com,https://example.com/login,alice,\"p,a\"\"ss\",\"line1\nline2\"\r\n\
            bank,https://bank.example/,bob,s3cret,\r\n\
            empty,https://x.example/,carol,,\r\n\
            \r\n";
        let (rows, skipped) = parse_logins(text).unwrap();
        assert_eq!(skipped, 1);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].url, "https://example.com/login");
        assert_eq!(rows[0].username, "alice");
        assert_eq!(rows[0].password.expose(), "p,a\"ss");
        assert_eq!(rows[0].note, "line1\nline2");
        assert_eq!(rows[1].password.expose(), "s3cret");
    }

    #[test]
    fn firefox_and_bitwarden_headers() {
        let ff = "\"url\",\"username\",\"password\",\"httpRealm\",\"formActionOrigin\"\n\
                  \"https://a.example\",\"u\",\"p\",,\"https://a.example\"\n";
        let (rows, _) = parse_logins(ff).unwrap();
        assert_eq!(
            (rows[0].url.as_str(), rows[0].username.as_str()),
            ("https://a.example", "u")
        );
        let bw = "folder,favorite,type,name,notes,fields,reprompt,login_uri,login_username,login_password,login_totp\n\
                  ,,login,Site,,,0,https://b.example,bob,pw,\n";
        let (rows, _) = parse_logins(bw).unwrap();
        assert_eq!(rows[0].password.expose(), "pw");
        assert_eq!(rows[0].name, "Site");
        assert_eq!(parse_logins("a,b\n1,2\n").unwrap_err(), CsvError::NoColumns);
        assert_eq!(parse_logins("").unwrap_err(), CsvError::Empty);
        assert_eq!(
            parse_logins("url,password\n\"x,y\n").unwrap_err(),
            CsvError::Unterminated
        );
    }

    #[test]
    fn export_then_import_round_trip() {
        let rows = vec![
            CsvLogin {
                name: "example.com".into(),
                url: "https://example.com".into(),
                username: "a\"b".into(),
                password: Secret::new("=1+2, \"x\"\nz"),
                note: " spaced ".into(),
            },
            CsvLogin {
                name: "n".into(),
                url: "https://n.example".into(),
                username: "".into(),
                password: Secret::new("plain"),
                note: "".into(),
            },
        ];
        let text = write_logins(&rows);
        assert!(text.starts_with("name,url,username,password,note\r\n"));
        let (back, skipped) = parse_logins(&text).unwrap();
        assert_eq!(skipped, 0);
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].username, "a\"b");
        assert_eq!(back[0].password.expose(), "=1+2, \"x\"\nz");
        assert_eq!(back[0].note, " spaced ");
        assert_eq!(back[1].password.expose(), "plain");
    }
}
