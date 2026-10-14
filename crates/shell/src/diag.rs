// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! Start-up diagnostics: log file, panic hook, fatal-error dialog, and (Windows) console
//! handling.
//!
//! Release builds on Windows use the GUI subsystem (no console window). There, unless the
//! process already has a usable stderr (redirected by a script or CI) or `--console` /
//! `MYTHBROWSE_CONSOLE=1` asks for a console, stdout and stderr (Servo's log output, Rust
//! panic messages) are redirected into `<profile>/logs/mythbrowse.log`, i.e.
//! `%LOCALAPPDATA%\MythBrowse Open Build\logs\mythbrowse.log` (`MythBrowse` in official builds). A panic on the main thread and every
//! fatal start-up error are also written there and shown in a native error dialog, so the
//! browser never just vanishes.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::OnceLock;

const LOG_NAME: &str = "mythbrowse.log";
const TITLE: &str = "MythBrowse";

static LOG: OnceLock<Option<(PathBuf, Mutex<File>)>> = OnceLock::new();

/// Command-line flags handled here (removed from the arguments the browser sees).
pub struct Flags {
    #[cfg_attr(not(windows), allow(dead_code))]
    pub console: bool,
}

/// Strip diagnostic flags from `args`.
pub fn take_flags(args: &mut Vec<String>) -> Flags {
    let before = args.len();
    args.retain(|a| a != "--console");
    let env_console = std::env::var("MYTHBROWSE_CONSOLE")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    Flags {
        console: args.len() != before || env_console,
    }
}

/// `<profile>/logs`, falling back to the temp dir.
fn log_dir() -> PathBuf {
    mythbrowse_storage::default_profile_dir()
        .map(|d| d.join("logs"))
        .unwrap_or_else(|| {
            std::env::temp_dir()
                .join(mythbrowse_storage::PROFILE_DIR_NAME)
                .join("logs")
        })
}

/// `<profile>/perf.log` (`%LOCALAPPDATA%\MythBrowse Open Build\perf.log` on Windows): the perf trace
/// (`perf_host`), appended across runs.
pub fn perf_log_path() -> PathBuf {
    mythbrowse_storage::default_profile_dir()
        .unwrap_or_else(|| std::env::temp_dir().join(mythbrowse_storage::PROFILE_DIR_NAME))
        .join("perf.log")
}

/// Seconds.millis since the Unix epoch (log line prefix).
pub fn stamp() -> String {
    timestamp()
}

/// Open the log file (truncated each start; the previous run is kept as `.1`) and install the
/// panic hook. Call first thing in `main`.
pub fn init(flags: &Flags) {
    let dir = log_dir();
    let file = std::fs::create_dir_all(&dir).ok().and_then(|_| {
        let path = dir.join(LOG_NAME);
        let _ = std::fs::rename(&path, dir.join(format!("{LOG_NAME}.1")));
        OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)
            .ok()
            .map(|f| (path, f))
    });

    #[cfg(windows)]
    let file = win::setup_std_handles(flags.console, file);
    #[cfg(not(windows))]
    let _ = flags;

    let _ = LOG.set(file.map(|(p, f)| (p, Mutex::new(f))));
    write_line(&format!(
        "MythBrowse {} starting ({} {}), args: {:?}",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        std::env::args().skip(1).collect::<Vec<_>>()
    ));

    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        let name = thread.name().unwrap_or("<unnamed>").to_owned();
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_owned())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "<non-string panic payload>".to_owned());
        let location = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_default();
        let backtrace = std::backtrace::Backtrace::force_capture();
        write_line(&format!(
            "PANIC in thread '{name}' at {location}: {message}\n{backtrace}"
        ));
        default_hook(info);
        if name == "main" {
            let hint = hint_for(&message);
            show_error(&format!(
                "MythBrowse crashed while starting or running.\n\n{message}\n({location})\n\n{hint}Details were written to:\n{}",
                log_path_display()
            ));
        }
    }));
}

fn hint_for(message: &str) -> &'static str {
    let m = message.to_ascii_lowercase();
    if m.contains("egl") || m.contains("angle") {
        "Graphics initialisation failed: libEGL.dll / libGLESv2.dll (ANGLE) must be next to \
         mythbrowse.exe, and Direct3D 11 must be available. Re-extract the full download and \
         update your graphics driver.\n\n"
    } else if m.contains("rendering context") || m.contains("surfman") || m.contains("opengl") {
        "Graphics initialisation failed. Updating the graphics driver usually fixes this.\n\n"
    } else {
        ""
    }
}

fn log_path_display() -> String {
    match LOG.get().and_then(|l| l.as_ref()) {
        Some((path, _)) => path.display().to_string(),
        None => format!("(could not create a log file in {})", log_dir().display()),
    }
}

fn timestamp() -> String {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("{}.{:03}", ms / 1000, ms % 1000)
}

/// Append one line to the log file (no-op without one).
pub fn write_line(line: &str) {
    if let Some(Some((_, file))) = LOG.get()
        && let Ok(mut f) = file.lock()
    {
        let _ = writeln!(f, "[{}] {line}", timestamp());
        let _ = f.flush();
    }
}

/// Log, print and show an error dialog, then exit with status 1.
pub fn fatal(message: &str) -> ! {
    write_line(&format!("FATAL: {message}"));
    eprintln!("mythbrowse: fatal: {message}");
    show_error(&format!(
        "{message}\n\nDetails were written to:\n{}",
        log_path_display()
    ));
    std::process::exit(1);
}

/// A native error dialog (Windows). Elsewhere stderr already has the message.
/// Suppressed when `MYTHBROWSE_NO_DIALOG=1` (unattended runs such as CI).
pub fn show_error(text: &str) {
    if std::env::var("MYTHBROWSE_NO_DIALOG").is_ok_and(|v| v == "1") {
        return;
    }
    #[cfg(windows)]
    win::message_box(TITLE, text);
    #[cfg(not(windows))]
    let _ = (TITLE, text);
}

/// Windows: make sure ANGLE's `libEGL.dll` / `libGLESv2.dll` can be loaded before Servo's
/// surfman tries (it panics with "egl function was not loaded" otherwise).
pub fn check_graphics_runtime() {
    #[cfg(windows)]
    {
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|e| e.parent().map(std::path::Path::to_path_buf));
        let missing: Vec<&str> = ["libEGL.dll", "libGLESv2.dll"]
            .into_iter()
            .filter(|dll| exe_dir.as_ref().is_none_or(|d| !d.join(dll).is_file()))
            .collect();
        if !missing.is_empty() {
            fatal(&format!(
                "MythBrowse cannot start: {} missing next to mythbrowse.exe{}.\n\nThese ANGLE \
                 graphics libraries ship in the MythBrowse download; extract the whole zip \
                 (not only the .exe) and run it from that folder.",
                missing.join(" and "),
                exe_dir
                    .as_ref()
                    .map(|d| format!(" ({})", d.display()))
                    .unwrap_or_default()
            ));
        }
        if let Err(e) = win::load_library("libEGL.dll") {
            fatal(&format!(
                "MythBrowse cannot start: libEGL.dll exists but could not be loaded ({e}).\n\n\
                 It needs Direct3D (d3dcompiler_47.dll) and the Microsoft Visual C++ runtime; \
                 install Windows updates or the VC++ 2015-2022 redistributable."
            ));
        }
        write_line("graphics runtime: libEGL.dll + libGLESv2.dll present and loadable");
    }
}

#[cfg(windows)]
mod win {
    use std::fs::File;
    use std::os::windows::io::AsRawHandle;
    use std::path::PathBuf;

    use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Console::{
        ATTACH_PARENT_PROCESS, AllocConsole, AttachConsole, GetStdHandle, STD_ERROR_HANDLE,
        STD_OUTPUT_HANDLE, SetStdHandle,
    };
    use windows_sys::Win32::System::LibraryLoader::LoadLibraryW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        MB_ICONERROR, MB_OK, MB_SETFOREGROUND, MessageBoxW,
    };

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn usable(h: HANDLE) -> bool {
        !h.is_null() && h != INVALID_HANDLE_VALUE
    }

    /// Console mode: attach to the parent console (or open one). Otherwise, if stderr is
    /// not already redirected somewhere, send stdout + stderr into the log file.
    /// Returns the log file for the panic hook (a duplicate handle when std is redirected).
    pub fn setup_std_handles(
        console: bool,
        file: Option<(PathBuf, File)>,
    ) -> Option<(PathBuf, File)> {
        unsafe {
            if console {
                if AttachConsole(ATTACH_PARENT_PROCESS) == 0 {
                    AllocConsole();
                }
                return file;
            }
            if usable(GetStdHandle(STD_ERROR_HANDLE)) {
                return file; // launched with redirected output (script / CI): keep it
            }
            let (path, f) = file?;
            let std_copy = f.try_clone().ok();
            if let Some(copy) = std_copy {
                let h = copy.as_raw_handle() as HANDLE;
                SetStdHandle(STD_OUTPUT_HANDLE, h);
                SetStdHandle(STD_ERROR_HANDLE, h);
                std::mem::forget(copy); // owned by the process std handles from now on
            }
            Some((path, f))
        }
    }

    pub fn message_box(title: &str, text: &str) {
        let (t, m) = (wide(title), wide(text));
        unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                m.as_ptr(),
                t.as_ptr(),
                MB_OK | MB_ICONERROR | MB_SETFOREGROUND,
            );
        }
    }

    pub fn load_library(name: &str) -> Result<(), std::io::Error> {
        let w = wide(name);
        let h = unsafe { LoadLibraryW(w.as_ptr()) };
        if h.is_null() {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(()) // keep it loaded; surfman loads the same module next
        }
    }
}
