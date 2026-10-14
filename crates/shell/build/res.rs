// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! A compiled Windows resource file (`.res`, 32-bit format) with an icon group and a version
//! block, built from a `.ico`. Used by `build.rs`; tested on any host with
//! `CARGO_MANIFEST_DIR=$PWD rustc --edition 2024 --test build/res.rs` (from crates/shell).

/// RT_ICON / RT_GROUP_ICON / RT_VERSION.
const RT_ICON: u16 = 3;
const RT_GROUP_ICON: u16 = 14;
const RT_VERSION: u16 = 16;
/// English (United States).
const LANG: u16 = 0x0409;

/// One image of a `.ico` (its 16-byte directory entry minus the offset, plus its data).
#[derive(Debug, Clone, PartialEq)]
pub struct IcoImage {
    pub width: u8,
    pub height: u8,
    pub colors: u8,
    pub planes: u16,
    pub bit_count: u16,
    pub data: Vec<u8>,
}

fn u16_at(b: &[u8], i: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*b.get(i)?, *b.get(i + 1)?]))
}
fn u32_at(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_le_bytes([
        *b.get(i)?,
        *b.get(i + 1)?,
        *b.get(i + 2)?,
        *b.get(i + 3)?,
    ]))
}

/// Parse a `.ico` (ICONDIR + ICONDIRENTRY[] + image data).
pub fn parse_ico(b: &[u8]) -> Result<Vec<IcoImage>, String> {
    if u16_at(b, 0) != Some(0) || u16_at(b, 2) != Some(1) {
        return Err("not an .ico file".into());
    }
    let n = u16_at(b, 4).ok_or("truncated .ico")? as usize;
    if n == 0 {
        return Err("empty .ico".into());
    }
    let mut out = Vec::with_capacity(n);
    for k in 0..n {
        let e = 6 + 16 * k;
        let bad = || format!("bad .ico entry {k}");
        let size = u32_at(b, e + 8).ok_or_else(bad)? as usize;
        let off = u32_at(b, e + 12).ok_or_else(bad)? as usize;
        let data = b
            .get(off..off.checked_add(size).ok_or_else(bad)?)
            .ok_or_else(bad)?;
        out.push(IcoImage {
            width: b[e],
            height: b[e + 1],
            colors: b[e + 2],
            planes: u16_at(b, e + 4).ok_or_else(bad)?,
            bit_count: u16_at(b, e + 6).ok_or_else(bad)?,
            data: data.to_vec(),
        });
    }
    Ok(out)
}

fn pad4(v: &mut Vec<u8>) {
    while v.len() % 4 != 0 {
        v.push(0);
    }
}

/// One resource entry: header (type and name as ordinals) then data, DWORD-aligned.
fn entry(out: &mut Vec<u8>, ty: u16, name: u16, flags: u16, data: &[u8]) {
    let mut h = Vec::with_capacity(32);
    h.extend((data.len() as u32).to_le_bytes()); // DataSize
    h.extend(32u32.to_le_bytes()); // HeaderSize
    h.extend([0xFF, 0xFF]);
    h.extend(ty.to_le_bytes());
    h.extend([0xFF, 0xFF]);
    h.extend(name.to_le_bytes());
    h.extend(0u32.to_le_bytes()); // DataVersion
    h.extend(flags.to_le_bytes()); // MemoryFlags
    h.extend(LANG.to_le_bytes());
    h.extend(0u32.to_le_bytes()); // Version
    h.extend(0u32.to_le_bytes()); // Characteristics
    debug_assert_eq!(h.len(), 32);
    out.extend(h);
    out.extend(data);
    pad4(out);
}

fn utf16z(s: &str) -> Vec<u8> {
    s.encode_utf16()
        .chain(Some(0))
        .flat_map(u16::to_le_bytes)
        .collect()
}

/// A VS_VERSIONINFO-style node: wLength, wValueLength, wType, szKey, padding, value,
/// padding, children. `text`: the value is a string (wValueLength counts UTF-16 units).
fn node(key: &str, value: &[u8], text: bool, children: &[Vec<u8>]) -> Vec<u8> {
    let mut v = vec![0u8; 6];
    v.extend(utf16z(key));
    pad4(&mut v);
    v.extend(value);
    let value_len = if text { value.len() / 2 } else { value.len() };
    for c in children {
        pad4(&mut v);
        v.extend(c);
    }
    let len = v.len() as u16;
    v[0..2].copy_from_slice(&len.to_le_bytes());
    v[2..4].copy_from_slice(&(value_len as u16).to_le_bytes());
    v[4..6].copy_from_slice(&(if text { 1u16 } else { 0 }).to_le_bytes());
    v
}

/// "0.0.1" -> (major << 16 | minor, patch << 16 | build).
fn version_words(version: &str) -> (u32, u32) {
    let mut p = version
        .split(['.', '-', '+'])
        .map(|x| x.parse::<u16>().unwrap_or(0))
        .chain(std::iter::repeat(0));
    let (a, b, c, d) = (
        p.next().unwrap_or(0) as u32,
        p.next().unwrap_or(0) as u32,
        p.next().unwrap_or(0) as u32,
        0u32,
    );
    let _ = p.next();
    ((a << 16) | b, (c << 16) | d)
}

fn version_info(version: &str) -> Vec<u8> {
    let (ms, ls) = version_words(version);
    let mut fixed = Vec::new();
    for w in [
        0xFEEF04BDu32, // dwSignature
        0x0001_0000,   // dwStrucVersion
        ms,
        ls, // file version
        ms,
        ls,          // product version
        0x3F,        // dwFileFlagsMask
        0,           // dwFileFlags
        0x0004_0004, // VOS_NT_WINDOWS32
        1,           // VFT_APP
        0,
        0,
        0, // subtype, date
    ] {
        fixed.extend(w.to_le_bytes());
    }
    let s = |k: &str, v: &str| node(k, &utf16z(v), true, &[]);
    let strings = node(
        "040904B0",
        &[],
        true,
        &[
            s("CompanyName", "JustMyth"),
            s("FileDescription", "MythBrowse"),
            s("FileVersion", version),
            s("InternalName", "mythbrowse"),
            s(
                "LegalCopyright",
                "Copyright (c) 2026 JustMyth / Tan Ming Jing",
            ),
            s("OriginalFilename", "mythbrowse.exe"),
            s("ProductName", "MythBrowse"),
            s("ProductVersion", version),
        ],
    );
    let sfi = node("StringFileInfo", &[], true, &[strings]);
    let var = node("Translation", &[0x09, 0x04, 0xB0, 0x04], false, &[]);
    let vfi = node("VarFileInfo", &[], true, &[var]);
    node("VS_VERSION_INFO", &fixed, false, &[sfi, vfi])
}

/// The whole `.res`: the empty leading entry, every icon image, the group, the version.
pub fn build(ico: &[u8], version: &str) -> Result<Vec<u8>, String> {
    let images = parse_ico(ico)?;
    let mut out = Vec::new();
    entry(&mut out, 0, 0, 0, &[]); // marks a 32-bit .res file
    let mut group = Vec::new();
    group.extend(0u16.to_le_bytes());
    group.extend(1u16.to_le_bytes());
    group.extend((images.len() as u16).to_le_bytes());
    for (i, img) in images.iter().enumerate() {
        let id = (i + 1) as u16;
        entry(&mut out, RT_ICON, id, 0x1010, &img.data);
        group.extend([img.width, img.height, img.colors, 0]);
        group.extend(img.planes.to_le_bytes());
        group.extend(img.bit_count.to_le_bytes());
        group.extend((img.data.len() as u32).to_le_bytes());
        group.extend(id.to_le_bytes());
    }
    entry(&mut out, RT_GROUP_ICON, 1, 0x1030, &group);
    entry(&mut out, RT_VERSION, 1, 0x0030, &version_info(version));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Walk a .res file: (type, name, data) per entry.
    fn walk(r: &[u8]) -> Vec<(u16, u16, Vec<u8>)> {
        let mut i = 0;
        let mut out = Vec::new();
        while i < r.len() {
            let size = u32_at(r, i).unwrap() as usize;
            let head = u32_at(r, i + 4).unwrap() as usize;
            assert_eq!(head, 32);
            assert_eq!(u16_at(r, i + 8), Some(0xFFFF));
            let ty = u16_at(r, i + 10).unwrap();
            let name = u16_at(r, i + 14).unwrap();
            out.push((ty, name, r[i + head..i + head + size].to_vec()));
            i += head + size;
            i = (i + 3) & !3;
        }
        assert_eq!(i, r.len());
        out
    }

    #[test]
    fn real_icon_builds_a_valid_res() {
        let ico = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../packaging/windows/mythbrowse.ico"
        ))
        .or_else(|_| std::fs::read("../../packaging/windows/mythbrowse.ico"))
        .unwrap();
        let imgs = parse_ico(&ico).unwrap();
        assert_eq!(imgs.len(), 10);
        let res = build(&ico, "0.0.1").unwrap();
        let e = walk(&res);
        assert_eq!(e[0], (0, 0, vec![]));
        let icons: Vec<_> = e.iter().filter(|x| x.0 == RT_ICON).collect();
        assert_eq!(icons.len(), 10);
        assert!(icons.iter().all(|x| x.2.starts_with(b"\x89PNG")));
        let group = &e.iter().find(|x| x.0 == RT_GROUP_ICON).unwrap().2;
        assert_eq!(group.len(), 6 + 14 * 10);
        assert_eq!(u16_at(group, 4), Some(10));
        // Last entry is the 256 px image (width byte 0), id 10.
        assert_eq!(group[6 + 14 * 9], 0);
        assert_eq!(u16_at(group, 6 + 14 * 9 + 12), Some(10));
        let ver = &e.iter().find(|x| x.0 == RT_VERSION).unwrap().2;
        assert_eq!(u16_at(ver, 0).unwrap() as usize, ver.len());
        assert_eq!(u16_at(ver, 2), Some(52));
        let txt: Vec<u16> = ver
            .chunks(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        let s = String::from_utf16_lossy(&txt);
        // Key, then zero padding to a DWORD boundary, then the value.
        let at = s.find("FileDescription\0").unwrap() + "FileDescription\0".len();
        assert!(s[at..].trim_start_matches('\0').starts_with("MythBrowse\0"));
        assert!(s.contains("VS_VERSION_INFO"));
    }

    #[test]
    fn versions_pack_into_words() {
        assert_eq!(version_words("0.0.1"), (0, 1 << 16));
        assert_eq!(version_words("1.2.3"), ((1 << 16) | 2, 3 << 16));
        assert_eq!(version_words("0.3.0-beta"), (3, 0));
    }

    #[test]
    fn rejects_non_icons() {
        assert!(parse_ico(b"\x89PNG....").is_err());
        assert!(parse_ico(&[0, 0, 1, 0, 0, 0]).is_err());
    }
}
