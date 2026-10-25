//! Small Win32 helpers: fullscreen detection, process utilities, icons, autostart, single instance.

use image::RgbaImage;
use std::path::{Path, PathBuf};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HWND, RECT,
};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS,
    DeleteDC, DeleteObject, GetDC, GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO,
    MonitorFromWindow, ReleaseDC, SelectObject,
};
use windows::Win32::System::Threading::{
    CreateEventW, CreateMutexW, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    QueryFullProcessImageNameW, SetEvent, WaitForSingleObject,
};
use windows::Win32::UI::Shell::{
    QUNS_PRESENTATION_MODE, QUNS_RUNNING_D3D_FULL_SCREEN, SHQueryUserNotificationState,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DI_NORMAL, DestroyIcon, DrawIconEx, GWL_STYLE, GetClassNameW, GetForegroundWindow,
    GetWindowLongW, GetWindowRect, GetWindowThreadProcessId, HICON, PrivateExtractIconsW,
    WS_CAPTION, WS_MAXIMIZE, WS_POPUP,
};
use windows::core::{PWSTR, w};

pub fn begin_timer_resolution() {
    unsafe {
        windows::Win32::Media::timeBeginPeriod(1);
    }
}

pub fn end_timer_resolution() {
    unsafe {
        windows::Win32::Media::timeEndPeriod(1);
    }
}

pub fn process_image_path(pid: u32) -> Option<PathBuf> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok =
            QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len)
                .is_ok();
        let _ = CloseHandle(h);
        ok.then(|| PathBuf::from(String::from_utf16_lossy(&buf[..len as usize])))
    }
}

/// Returns the foreground app name when a game / video is truly fullscreen (exclusive or
/// borderless) on its monitor. Ordinary maximized windows with a title bar are ignored.
pub fn fullscreen_app() -> Option<String> {
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_invalid() {
            return None;
        }
        let mut cls = [0u16; 256];
        let n = GetClassNameW(hwnd, &mut cls).max(0) as usize;
        let cls = String::from_utf16_lossy(&cls[..n]).to_lowercase();
        if matches!(
            cls.as_str(),
            "progman"
                | "workerw"
                | "shell_traywnd"
                | "shell_secondarytraywnd"
                | "cortana"
                | "windows.ui.core.corewindow"
        ) {
            return None;
        }

        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == std::process::id() {
            return None;
        }
        let name = || {
            process_image_path(pid)
                .and_then(|p| p.file_name().map(|f| f.to_string_lossy().into_owned()))
                .unwrap_or_else(|| "Fullscreen app".into())
        };
        let ignored = |n: &str| {
            matches!(
                n.to_lowercase().as_str(),
                "explorer.exe" | "vdisplay.exe" | "searchhost.exe" | "startmenuexperiencehost.exe"
            )
        };

        // Direct3D exclusive fullscreen / presentation mode reported by the shell.
        if let Ok(state) = SHQueryUserNotificationState()
            && (state == QUNS_RUNNING_D3D_FULL_SCREEN || state == QUNS_PRESENTATION_MODE)
        {
            let n = name();
            return (!ignored(&n)).then_some(n);
        }

        let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
        let caption = style & WS_CAPTION.0;
        if style & WS_MAXIMIZE.0 != 0 && caption != 0 {
            return None;
        }
        let mut r = RECT::default();
        GetWindowRect(hwnd, &mut r).ok()?;
        let mon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(mon, &mut mi).as_bool() {
            return None;
        }
        let m = mi.rcMonitor;
        let covers =
            r.left <= m.left && r.top <= m.top && r.right >= m.right && r.bottom >= m.bottom;
        let borderless = caption != WS_CAPTION.0 || style & WS_POPUP.0 != 0;
        if covers && borderless {
            let n = name();
            if !ignored(&n) {
                return Some(n);
            }
        }
        None
    }
}

/// Finds the executable of a running process whose name matches a media session id
/// (e.g. `Spotify.exe`, `Chrome`, `MSEdge`).
pub fn find_process_exe(source_key: &str) -> Option<PathBuf> {
    let key = source_key.to_lowercase();
    let stem = key
        .trim_end_matches(".exe")
        .split(['!', '_'])
        .next()
        .unwrap_or("")
        .to_string();
    if stem.is_empty() {
        return None;
    }
    let mut sys = sysinfo::System::new();
    sys.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::All,
        true,
        sysinfo::ProcessRefreshKind::nothing().with_exe(sysinfo::UpdateKind::OnlyIfNotSet),
    );
    let mut best: Option<PathBuf> = None;
    for p in sys.processes().values() {
        let name = p.name().to_string_lossy().to_lowercase();
        let pname = name.trim_end_matches(".exe");
        if pname.is_empty() {
            continue;
        }
        if (pname == stem || pname.contains(&stem) || stem.contains(pname))
            && let Some(exe) = p.exe()
        {
            if pname == stem {
                return Some(exe.to_path_buf());
            }
            best.get_or_insert_with(|| exe.to_path_buf());
        }
    }
    best
}

/// Renders the executable's icon at `size`×`size` into straight RGBA.
pub fn extract_exe_icon(path: &Path, size: u32) -> Option<RgbaImage> {
    unsafe {
        let mut name = [0u16; 260];
        let w: Vec<u16> = path.as_os_str().to_string_lossy().encode_utf16().collect();
        if w.len() >= 260 {
            return None;
        }
        name[..w.len()].copy_from_slice(&w);
        let mut icons = [HICON::default()];
        let n = PrivateExtractIconsW(
            &name,
            0,
            size as i32,
            size as i32,
            Some(&mut icons),
            None,
            0,
        );
        if n == 0 || n == u32::MAX || icons[0].is_invalid() {
            return None;
        }
        let hicon = icons[0];

        let screen = GetDC(None);
        let dc = CreateCompatibleDC(Some(screen));
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: size as i32,
                biHeight: -(size as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let mut out = None;
        if let Ok(bmp) = CreateDIBSection(Some(dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
            let old = SelectObject(dc, bmp.into());
            if DrawIconEx(
                dc,
                0,
                0,
                hicon,
                size as i32,
                size as i32,
                0,
                None,
                DI_NORMAL,
            )
            .is_ok()
                && !bits.is_null()
            {
                let px = std::slice::from_raw_parts(bits as *const u8, (size * size * 4) as usize);
                let no_alpha = px.chunks_exact(4).all(|p| p[3] == 0);
                let mut img = RgbaImage::new(size, size);
                for (o, p) in img.pixels_mut().zip(px.chunks_exact(4)) {
                    // GDI leaves alpha at 0 for legacy icons; treat those as opaque.
                    let a = if no_alpha { 255 } else { p[3] };
                    // DIB data is premultiplied BGRA; un-premultiply.
                    let un = |c: u8| {
                        if a == 0 {
                            0
                        } else {
                            ((c as u32 * 255 + a as u32 / 2) / a as u32).min(255) as u8
                        }
                    };
                    *o = if no_alpha {
                        image::Rgba([p[2], p[1], p[0], 255])
                    } else {
                        image::Rgba([un(p[2]), un(p[1]), un(p[0]), a])
                    };
                }
                out = Some(img);
            }
            SelectObject(dc, old);
            let _ = DeleteObject(bmp.into());
        }
        let _ = DeleteDC(dc);
        ReleaseDC(None, screen);
        let _ = DestroyIcon(hicon);
        out
    }
}

/// Removes HWiNFO's notification-area icon. HWiNFO has no setting for this, so the icon is
/// deleted from the shell directly (it only re-adds it when HWiNFO or Explorer restarts).
/// Returns true when an icon was removed.
pub fn hide_hwinfo_tray_icon() -> bool {
    use windows::Win32::Foundation::{LPARAM, TRUE};
    use windows::Win32::UI::Shell::{NIM_DELETE, NOTIFYICONDATAW, Shell_NotifyIconW};
    use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, GetWindowTextW};
    use windows::core::BOOL;

    unsafe extern "system" fn visit(hwnd: HWND, lp: LPARAM) -> BOOL {
        unsafe {
            let mut title = [0u16; 64];
            let n = GetWindowTextW(hwnd, &mut title).max(0) as usize;
            if !String::from_utf16_lossy(&title[..n]).starts_with("HWiNFO") {
                return TRUE;
            }
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            let is_hwinfo = process_image_path(pid)
                .and_then(|p| p.file_name().map(|f| f.to_string_lossy().to_lowercase()))
                .is_some_and(|f| f.starts_with("hwinfo"));
            if is_hwinfo {
                // HWiNFO registers its app icon with uID 0xFFFFFFFE; per-sensor icons are left alone.
                let nid = NOTIFYICONDATAW {
                    cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
                    hWnd: hwnd,
                    uID: 0xFFFF_FFFE,
                    ..Default::default()
                };
                if Shell_NotifyIconW(NIM_DELETE, &nid).as_bool() {
                    *(lp.0 as *mut bool) = true;
                }
            }
            TRUE
        }
    }

    let mut removed = false;
    unsafe {
        let _ = EnumWindows(Some(visit), LPARAM(&mut removed as *mut bool as isize));
    }
    removed
}

/// Force-closes the vendor software that holds the COM port. Returns how many were killed.
pub fn kill_processes(names: &[&str]) -> usize {
    if crate::device::dry_run() {
        return 0;
    }
    let mut sys = sysinfo::System::new();
    sys.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::All,
        true,
        sysinfo::ProcessRefreshKind::nothing(),
    );
    let mut n = 0;
    for p in sys.processes().values() {
        let pname = p.name().to_string_lossy();
        if names.iter().any(|t| pname.eq_ignore_ascii_case(t)) && p.kill() {
            n += 1;
        }
    }
    n
}

// Run at Windows startup (HKCU\...\Run)

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_NAME: &str = "Vdisplay";

pub fn run_at_startup_enabled() -> bool {
    let hkcu = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER);
    hkcu.open_subkey(RUN_KEY)
        .and_then(|k| k.get_value::<String, _>(RUN_NAME))
        .is_ok()
}

pub fn set_run_at_startup(enable: bool) -> std::io::Result<()> {
    let hkcu = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER);
    let (key, _) = hkcu.create_subkey(RUN_KEY)?;
    if enable {
        let exe = std::env::current_exe()?;
        key.set_value(RUN_NAME, &format!("\"{}\" --minimized", exe.display()))
    } else {
        match key.delete_value(RUN_NAME) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }
}

// Single instance: a second launch just asks the first one to show its window.

pub struct SingleInstance {
    _mutex: HANDLE,
    event: HANDLE,
}

unsafe impl Send for SingleInstance {}
unsafe impl Sync for SingleInstance {}

impl SingleInstance {
    /// `None` if another instance is running (it has been asked to show itself).
    pub fn acquire() -> Option<Self> {
        unsafe {
            let mutex = CreateMutexW(None, true, w!("Local\\VdisplayController.Instance")).ok()?;
            let already = GetLastError() == ERROR_ALREADY_EXISTS;
            let event =
                CreateEventW(None, false, false, w!("Local\\VdisplayController.Show")).ok()?;
            if already {
                let _ = SetEvent(event);
                let _ = CloseHandle(event);
                let _ = CloseHandle(mutex);
                return None;
            }
            Some(Self {
                _mutex: mutex,
                event,
            })
        }
    }

    /// Blocks until another launch requests the window (returns false after 1 s).
    pub fn wait_show_request(&self) -> bool {
        unsafe { WaitForSingleObject(self.event, 1000).0 == 0 }
    }
}

/// DWM-cloaks a window: it stays "shown" for the window manager but is never drawn.
/// Used to start hidden without the first-frame flash eframe would otherwise cause.
pub fn set_cloaked(hwnd: isize, cloak: bool) {
    use windows::Win32::Graphics::Dwm::{DWMWA_CLOAK, DwmSetWindowAttribute};
    let v: windows::core::BOOL = cloak.into();
    unsafe {
        let _ = DwmSetWindowAttribute(
            HWND(hwnd as *mut core::ffi::c_void),
            DWMWA_CLOAK,
            &v as *const _ as *const core::ffi::c_void,
            std::mem::size_of_val(&v) as u32,
        );
    }
}
