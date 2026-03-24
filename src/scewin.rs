use std::fs;
use std::io;
use std::mem::{size_of, zeroed};
use std::os::windows::process::ExitStatusExt;
use std::path::{Path, PathBuf};

// SCEWIN bytes live in OUT_DIR/scewin_mirror at compile time (see build.rs).
mod bundled {
    include!(concat!(env!("OUT_DIR"), "/scewin_bundled.rs"));
}
use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject, INFINITE};
use windows_sys::Win32::UI::Shell::{ShellExecuteExW, SHELLEXECUTEINFOW, SEE_MASK_NOCLOSEPROCESS};
use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;

/// Folder where SCEWIN_64.exe lives (often a version subdir under SCEWIN).
///
/// Order: folder next to exe → cwd → dev tree → **embedded** copy under
/// `%LOCALAPPDATA%\nvram_editor\scewin_embedded\` (only if you built with `SCEWIN/` present).
pub fn resolve_scewin_work_dir() -> Option<PathBuf> {
    for root in scewin_search_roots() {
        if let Some(w) = work_dir_in_bundle(&root) {
            return Some(w);
        }
    }
    if bundled::BUNDLED {
        if let Some(base) = runtime_embed_root() {
            let _ = ensure_embedded_bundle(&base);
            if let Some(w) = work_dir_in_bundle(&base) {
                return Some(w);
            }
        }
    }
    None
}

fn runtime_embed_root() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|p| {
        PathBuf::from(p)
            .join("nvram_editor")
            .join("scewin_embedded")
    })
}

fn ensure_embedded_bundle(base: &Path) -> io::Result<()> {
    if !bundled::BUNDLED {
        return Ok(());
    }
    let marker = base.join(".bundle_version");
    let want_ver = env!("CARGO_PKG_VERSION");
    let up_to_date = marker.is_file()
        && fs::read_to_string(&marker)
            .map(|s| s.trim() == want_ver)
            .unwrap_or(false)
        && work_dir_in_bundle(base).is_some();
    if up_to_date {
        return Ok(());
    }
    // Never wipe the tree: nvram.txt and logs may live here. Only (re)write bundled binaries.
    fs::create_dir_all(base)?;
    bundled::extract_embedded(base)?;
    fs::write(marker, want_ver)?;
    Ok(())
}

fn scewin_search_roots() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            out.push(dir.to_path_buf());
            out.push(dir.join("SCEWIN"));
        }
    }
    if let Some(dir) = std::env::current_dir().ok() {
        out.push(dir.clone());
        out.push(dir.join("SCEWIN"));
    }
    #[cfg(debug_assertions)]
    {
        let dev = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("SCEWIN");
        out.push(dev);
    }
    out
}

fn work_dir_in_bundle(scewin_root: &Path) -> Option<PathBuf> {
    if !scewin_root.is_dir() {
        return None;
    }
    if scewin_root.join("SCEWIN_64.exe").is_file() {
        return Some(scewin_root.to_path_buf());
    }
    // Common SCEHUB layout: the executable can be inside one more level.
    if let Some(found) = find_scewin_dir_recursive(scewin_root, 2) {
        return Some(found);
    }
    let rd = std::fs::read_dir(scewin_root).ok()?;
    let mut candidates: Vec<PathBuf> = Vec::new();
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() && p.join("SCEWIN_64.exe").is_file() {
            candidates.push(p);
        }
    }
    candidates.sort();
    candidates.into_iter().next()
}

fn find_scewin_dir_recursive(root: &Path, max_depth: usize) -> Option<PathBuf> {
    if root.join("SCEWIN_64.exe").is_file() {
        return Some(root.to_path_buf());
    }
    if max_depth == 0 {
        return None;
    }
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(root)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    for d in dirs {
        if let Some(found) = find_scewin_dir_recursive(&d, max_depth - 1) {
            return Some(found);
        }
    }
    None
}

pub fn nvram_path_in_work(work: &Path) -> PathBuf {
    work.join("nvram.txt")
}

/// `true` if this binary was built with a `SCEWIN/` tree (exe + sys embedded inside).
pub fn has_embedded_scewin() -> bool {
    bundled::BUNDLED
}

/// Launch exe as admin. No .bat — some machines block those and pause() is annoying anyway.
pub fn run_scewin_elevated(work: &Path, args: &[&str]) -> io::Result<std::process::ExitStatus> {
    let exe = work.join("SCEWIN_64.exe");
    if !exe.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "SCEWIN_64.exe not found in {}. Copy SCEWIN (exe + .sys), e.g. from SCEHUB.",
                work.display()
            ),
        ));
    }

    let exe_w = to_wide(&exe.to_string_lossy());
    let work_w = to_wide(&work.to_string_lossy());
    let verb_w = to_wide("runas");
    let params = args.join(" ");
    let params_w = to_wide(&params);

    let mut sei: SHELLEXECUTEINFOW = unsafe { zeroed() };
    sei.cbSize = size_of::<SHELLEXECUTEINFOW>() as u32;
    sei.fMask = SEE_MASK_NOCLOSEPROCESS;
    sei.lpVerb = verb_w.as_ptr();
    sei.lpFile = exe_w.as_ptr();
    sei.lpParameters = if params.is_empty() {
        std::ptr::null()
    } else {
        params_w.as_ptr()
    };
    sei.lpDirectory = work_w.as_ptr();
    sei.nShow = SW_HIDE;

    let launched = unsafe { ShellExecuteExW(&mut sei) };
    if launched == 0 {
        return Err(io::Error::last_os_error());
    }
    if sei.hProcess.is_null() {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            "Elevated process started without process handle.",
        ));
    }

    let wait_result = unsafe { WaitForSingleObject(sei.hProcess, INFINITE) };
    if wait_result == 0xFFFF_FFFF {
        unsafe {
            CloseHandle(sei.hProcess);
        }
        return Err(io::Error::last_os_error());
    }

    let mut code: u32 = 0;
    let got_code = unsafe { GetExitCodeProcess(sei.hProcess, &mut code) };
    unsafe {
        CloseHandle(sei.hProcess);
    }
    if got_code == 0 {
        return Err(io::Error::last_os_error());
    }

    Ok(std::process::ExitStatus::from_raw(code))
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn export_nvram(work: &Path) -> io::Result<std::process::ExitStatus> {
    run_scewin_elevated(work, &["/o", "/s", "nvram.txt"])
}

pub fn import_nvram(work: &Path) -> io::Result<std::process::ExitStatus> {
    run_scewin_elevated(work, &["/i", "/s", "nvram.txt"])
}
