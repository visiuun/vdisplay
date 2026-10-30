//! Hardware sensors (CPU temperature, power, clocks, fans).
//!
//! Windows exposes none of these without a kernel driver, so we read them from a sensor app that
//! has one: HWiNFO (shared memory, preferred) or LibreHardwareMonitor / OpenHardwareMonitor (WMI).
//! For HWiNFO we can also do a one-time elevated setup (enable shared memory + register an
//! on-demand scheduled task) so Vdisplay can start/restart it later without UAC prompts.

use std::collections::HashMap;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
pub const TASK_NAME: &str = "Vdisplay HWiNFO";
/// Bumped when the task's action changes, so older tasks get set up again.
const TASK_DESC: &str = "Starts HWiNFO sensors for Vdisplay (v2)";

#[derive(Clone, Debug, PartialEq)]
pub struct Fan {
    pub name: String,
    pub rpm: f32,
}

#[derive(Clone, Default)]
pub struct SensorReading {
    pub cpu_temp: Option<f32>,
    pub cpu_power: Option<f32>,
    pub cpu_clock: Option<f32>,
    pub gpu_temp: Option<f32>,
    pub gpu_load: Option<f32>,
    pub fans: Vec<Fan>,
    pub source: &'static str,
}

/// What the UI should say about sensor availability.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum SensorState {
    #[default]
    Unknown,
    /// Reading live data from `SensorReading::source`.
    Live,
    /// HWiNFO is installed but Vdisplay hasn't been allowed to manage it yet.
    HwinfoNeedsSetup,
    /// Set up; HWiNFO is being started / restarted.
    HwinfoStarting,
    /// HWiNFO runs but shared memory is off (user-started without the setting).
    HwinfoNoSharedMemory,
    /// No supported sensor app installed.
    NoProvider,
}

// HWiNFO shared memory ("Global\HWiNFO_SENS_SM2", layout from hwisenssm2.h, #pragma pack(1))

const SIG_ACTIVE: u32 = u32::from_le_bytes(*b"HWiS");

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Temp,
    Fan,
    Power,
    Clock,
    Usage,
    Other,
}

#[derive(Debug)]
struct Reading {
    sensor: String,
    kind: Kind,
    label: String,
    value: f64,
    max: f64,
}

enum HwinfoSnapshot {
    Missing,
    Inactive,
    Stale,
    Ok(Vec<Reading>),
}

fn cstr(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).trim().to_string()
}

fn rd_u32(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?))
}

fn rd_f64(b: &[u8], o: usize) -> Option<f64> {
    Some(f64::from_le_bytes(b.get(o..o + 8)?.try_into().ok()?))
}

/// Parses a copy of the shared-memory block. Element sizes come from the header, so newer
/// HWiNFO versions that append fields keep working.
fn parse_hwinfo(b: &[u8], now_unix: i64) -> HwinfoSnapshot {
    let Some(sig) = rd_u32(b, 0) else {
        return HwinfoSnapshot::Missing;
    };
    if sig != SIG_ACTIVE {
        return HwinfoSnapshot::Inactive;
    }
    let poll = b
        .get(12..20)
        .map(|s| i64::from_le_bytes(s.try_into().unwrap()))
        .unwrap_or(0);
    if poll > 0 && (now_unix - poll).abs() > 30 {
        return HwinfoSnapshot::Stale;
    }
    let h = |o| rd_u32(b, o).unwrap_or(0) as usize;
    let (s_off, s_size, s_num) = (h(20), h(24), h(28));
    let (r_off, r_size, r_num) = (h(32), h(36), h(40));
    if s_size < 264 || r_size < 316 || s_num > 4096 || r_num > 65536 {
        return HwinfoSnapshot::Inactive;
    }
    let mut names = Vec::with_capacity(s_num);
    for i in 0..s_num {
        let o = s_off + i * s_size;
        let Some(e) = b.get(o..o + s_size) else { break };
        let user = cstr(&e[136..264]);
        names.push(if user.is_empty() {
            cstr(&e[8..136])
        } else {
            user
        });
    }
    let mut out = Vec::with_capacity(r_num);
    for i in 0..r_num {
        let o = r_off + i * r_size;
        let Some(e) = b.get(o..o + r_size) else { break };
        let kind = match rd_u32(e, 0).unwrap_or(0) {
            1 => Kind::Temp,
            3 => Kind::Fan,
            5 => Kind::Power,
            6 => Kind::Clock,
            7 => Kind::Usage,
            _ => Kind::Other,
        };
        let sensor = names
            .get(rd_u32(e, 4).unwrap_or(0) as usize)
            .cloned()
            .unwrap_or_default();
        let user = cstr(&e[140..268]);
        let label = if user.is_empty() {
            cstr(&e[12..140])
        } else {
            user
        };
        let value = rd_f64(e, 284).unwrap_or(f64::NAN);
        let max = rd_f64(e, 300).unwrap_or(f64::NAN);
        if value.is_finite() {
            out.push(Reading {
                sensor,
                kind,
                label,
                value,
                max,
            });
        }
    }
    HwinfoSnapshot::Ok(out)
}

fn read_hwinfo() -> HwinfoSnapshot {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Memory::{
        FILE_MAP_READ, MEMORY_BASIC_INFORMATION, MapViewOfFile, OpenFileMappingW, UnmapViewOfFile,
        VirtualQuery,
    };
    unsafe {
        let Ok(h) = OpenFileMappingW(
            FILE_MAP_READ.0,
            false,
            windows::core::w!("Global\\HWiNFO_SENS_SM2"),
        ) else {
            return HwinfoSnapshot::Missing;
        };
        let view = MapViewOfFile(h, FILE_MAP_READ, 0, 0, 0);
        let snap = if view.Value.is_null() {
            HwinfoSnapshot::Missing
        } else {
            let mut mbi = MEMORY_BASIC_INFORMATION::default();
            VirtualQuery(
                Some(view.Value),
                &mut mbi,
                std::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
            );
            let len = mbi.RegionSize.min(32 * 1024 * 1024);
            // Copy out first so HWiNFO updating the block can't tear what we parse.
            let copy = std::slice::from_raw_parts(view.Value as *const u8, len).to_vec();
            let _ = UnmapViewOfFile(view);
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            parse_hwinfo(&copy, now)
        };
        let _ = CloseHandle(h);
        snap
    }
}

fn is_cpu_sensor(s: &str) -> bool {
    let s = s.to_lowercase();
    s.starts_with("cpu [") || s.starts_with("cpu[")
}

fn is_gpu_sensor(s: &str) -> bool {
    s.to_lowercase().starts_with("gpu [")
}

fn pick_hwinfo(rs: &[Reading]) -> SensorReading {
    let mut r = SensorReading {
        source: "HWiNFO",
        ..Default::default()
    };
    let mut temp_rank = 0;
    let mut power_rank = 0;
    let mut clock_rank = 0;
    for x in rs {
        let l = x.label.to_lowercase();
        let v = x.value as f32;
        match x.kind {
            Kind::Temp if is_cpu_sensor(&x.sensor) || l == "cpu" => {
                let rank = if l.contains("tctl/tdie") || l == "cpu package" {
                    5
                } else if l.contains("die (average)") || l.contains("tdie") || l.contains("tctl") {
                    4
                } else if l.contains("core max") {
                    3
                } else if is_cpu_sensor(&x.sensor) && l.starts_with("cpu") {
                    2
                } else {
                    1
                };
                if rank > temp_rank && v > 0.0 && v < 150.0 {
                    temp_rank = rank;
                    r.cpu_temp = Some(v);
                }
            }
            Kind::Temp if is_gpu_sensor(&x.sensor) && l == "gpu temperature" => {
                r.gpu_temp = Some(v)
            }
            Kind::Power if is_cpu_sensor(&x.sensor) => {
                let rank = if l == "cpu package power" {
                    3
                } else if l.contains("ppt") {
                    2
                } else {
                    0
                };
                if rank > power_rank {
                    power_rank = rank;
                    r.cpu_power = Some(v);
                }
            }
            Kind::Clock if is_cpu_sensor(&x.sensor) => {
                let rank = if l == "average effective clock" {
                    3
                } else if l.contains("core clocks (avg)") {
                    2
                } else {
                    0
                };
                if rank > clock_rank && v > 0.0 {
                    clock_rank = rank;
                    r.cpu_clock = Some(v);
                }
            }
            Kind::Usage if is_gpu_sensor(&x.sensor) && l == "gpu core load" => r.gpu_load = Some(v),
            Kind::Fan if x.value > 0.0 || x.max > 0.0 => {
                let name = if is_gpu_sensor(&x.sensor) && !l.starts_with("gpu") {
                    format!("GPU {}", x.label)
                } else {
                    x.label.clone()
                };
                if !r.fans.iter().any(|f| f.name == name) {
                    r.fans.push(Fan {
                        name,
                        rpm: v.max(0.0),
                    });
                }
            }
            _ => {}
        }
    }
    r
}

// LibreHardwareMonitor / OpenHardwareMonitor via WMI

type Row = HashMap<String, wmi::Variant>;

fn vf(v: Option<&wmi::Variant>) -> Option<f32> {
    match v? {
        wmi::Variant::R4(f) => Some(*f),
        wmi::Variant::R8(f) => Some(*f as f32),
        wmi::Variant::I4(i) => Some(*i as f32),
        wmi::Variant::UI4(i) => Some(*i as f32),
        _ => None,
    }
}

fn vs(v: Option<&wmi::Variant>) -> String {
    match v {
        Some(wmi::Variant::String(s)) => s.clone(),
        _ => String::new(),
    }
}

fn pick_lhm(rows: &[Row], label: &'static str) -> SensorReading {
    let mut r = SensorReading {
        source: label,
        ..Default::default()
    };
    let mut temp_rank = 0;
    for row in rows {
        let ty = vs(row.get("SensorType")).to_lowercase();
        let name = vs(row.get("Name"));
        let ln = name.to_lowercase();
        let id = vs(row.get("Identifier")).to_lowercase();
        let Some(v) = vf(row.get("Value")) else {
            continue;
        };
        let cpu = id.contains("cpu") && !id.contains("gpu");
        let gpu = id.contains("gpu");
        match ty.as_str() {
            "temperature" if cpu => {
                let rank = if ln.contains("package") || ln.contains("tctl") || ln.contains("tdie") {
                    3
                } else if ln.contains("average") {
                    2
                } else {
                    1
                };
                if rank > temp_rank && v > 0.0 {
                    temp_rank = rank;
                    r.cpu_temp = Some(v);
                }
            }
            "temperature" if gpu && ln.contains("core") => r.gpu_temp = Some(v),
            "power" if cpu && ln.contains("package") => r.cpu_power = Some(v),
            "load" if gpu && ln.contains("core") => r.gpu_load = Some(v),
            "fan" => {
                let n = if gpu && !ln.starts_with("gpu") {
                    format!("GPU {name}")
                } else {
                    name
                };
                r.fans.push(Fan {
                    name: n,
                    rpm: v.max(0.0),
                });
            }
            _ => {}
        }
    }
    r
}

// HWiNFO management

pub fn hwinfo_exe() -> Option<PathBuf> {
    use winreg::RegKey;
    use winreg::enums::{HKEY_LOCAL_MACHINE, KEY_READ};
    let mut candidates = Vec::new();
    for key in [
        r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\HWiNFO64_is1",
        r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\HWiNFO64_is1",
    ] {
        if let Ok(k) = RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey_with_flags(key, KEY_READ)
            && let Ok(loc) = k.get_value::<String, _>("InstallLocation")
        {
            candidates.push(PathBuf::from(loc).join("HWiNFO64.EXE"));
        }
    }
    for base in [
        std::env::var_os("ProgramFiles"),
        std::env::var_os("ProgramFiles(x86)"),
    ]
    .into_iter()
    .flatten()
    {
        candidates.push(PathBuf::from(base).join("HWiNFO64").join("HWiNFO64.EXE"));
    }
    candidates.into_iter().find(|p| p.is_file())
}

pub fn hwinfo_running() -> bool {
    let mut sys = sysinfo::System::new();
    sys.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::All,
        true,
        sysinfo::ProcessRefreshKind::nothing(),
    );
    sys.processes().values().any(|p| {
        p.name()
            .to_string_lossy()
            .to_lowercase()
            .starts_with("hwinfo")
    })
}

/// True if our task exists and is the current version (older ones never re-enabled SensorsSM).
pub fn task_installed() -> bool {
    std::process::Command::new("schtasks")
        .args(["/query", "/tn", TASK_NAME, "/xml"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .is_ok_and(|o| {
            o.status.success()
                && String::from_utf8_lossy(&o.stdout)
                    .replace('\0', "")
                    .contains(TASK_DESC)
        })
}

/// Starts (or restarts) HWiNFO elevated via the task, without a UAC prompt.
pub fn run_task() -> bool {
    if crate::device::dry_run() {
        return false;
    }
    std::process::Command::new("schtasks")
        .args(["/run", "/tn", TASK_NAME])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .is_ok_and(|o| o.status.success())
}

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                s.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                s.push('=');
            }
        }
    }
    s
}

/// PowerShell that turns on shared memory and tray-only start in `$exe`'s INI.
const INI_PATCH: &str = r#"$ini = Join-Path (Split-Path $exe) 'HWiNFO64.INI'
$want = [ordered]@{ SensorsOnly = '1'; SensorsSM = '1'; MinimalizeSensors = '1'; MinimalizeMainWnd = '1'; ShowWelcomeAndProgress = '0' }
$lines = @(if (Test-Path $ini) { Get-Content $ini })
if (-not ($lines -match '^\[Settings\]')) { $lines = @('[Settings]') + $lines }
$out = New-Object System.Collections.Generic.List[string]
$inSettings = $false
foreach ($l in $lines) {
  if ($l -match '^\[') {
    if ($inSettings) { foreach ($k in $want.Keys) { $out.Add("$k=$($want[$k])") }; $want.Clear() }
    $inSettings = ($l -eq '[Settings]')
    $out.Add($l); continue
  }
  if ($inSettings -and $l -match '^([^=]+)=') { if ($want.Contains($matches[1])) { continue } }
  $out.Add($l)
}
if ($inSettings) { foreach ($k in $want.Keys) { $out.Add("$k=$($want[$k])") } }
Set-Content -Path $ini -Value $out -Encoding ASCII"#;

/// Base64 UTF-16LE, as `powershell -EncodedCommand` expects.
fn encode_ps(script: &str) -> String {
    let utf16: Vec<u8> = script
        .encode_utf16()
        .flat_map(|u| u.to_le_bytes())
        .collect();
    base64(&utf16)
}

/// One-time setup (shows one UAC prompt): installs HWiNFO if needed and registers an on-demand
/// task that (re)starts it elevated with shared memory and tray-only start enabled in its INI.
pub fn setup_hwinfo() -> Result<(), String> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
    use windows::Win32::UI::Shell::{SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW};

    // Not installed yet: the same elevated script installs it with winget first.
    let installed = hwinfo_exe();
    let install = installed.is_none();
    let exe = installed.unwrap_or_else(|| {
        PathBuf::from(
            std::env::var_os("ProgramFiles").unwrap_or_else(|| "C:\\Program Files".into()),
        )
        .join("HWiNFO64")
        .join("HWiNFO64.EXE")
    });
    let exe = exe.to_string_lossy().replace('\'', "''");
    // HWiNFO rewrites its INI and can drop SensorsSM (e.g. when the free version's 12 h shared
    // memory limit expires), so the task re-enables it on every (re)start, not just once here.
    let start = encode_ps(&format!(
        r#"$exe = '{exe}'
Get-Process HWiNFO64 -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
Get-Process HWiNFO64 -ErrorAction SilentlyContinue | Wait-Process -Timeout 5 -ErrorAction SilentlyContinue
{INI_PATCH}
Start-Process $exe
"#
    ));
    let script = format!(
        r#"$ErrorActionPreference = 'Stop'
$exe = '{exe}'
if ('{install}' -eq 'true' -and -not (Test-Path $exe)) {{
  $winget = Get-Command winget -ErrorAction SilentlyContinue
  if (-not $winget) {{ exit 3 }}
  & $winget.Source install --id REALiX.HWiNFO -e --silent --scope machine --accept-package-agreements --accept-source-agreements | Out-Null
  if (-not (Test-Path $exe)) {{ exit 4 }}
  Get-Process HWiNFO64 -ErrorAction SilentlyContinue | Stop-Process -Force
}}
$action = New-ScheduledTaskAction -Execute 'powershell.exe' -Argument '-NoProfile -NonInteractive -WindowStyle Hidden -ExecutionPolicy Bypass -EncodedCommand {start}'
$principal = New-ScheduledTaskPrincipal -UserId ([Security.Principal.WindowsIdentity]::GetCurrent().Name) -LogonType Interactive -RunLevel Highest
$settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -ExecutionTimeLimit (New-TimeSpan -Minutes 5)
Register-ScheduledTask -TaskName '{TASK_NAME}' -Action $action -Principal $principal -Settings $settings -Description '{TASK_DESC}' -Force | Out-Null
Start-ScheduledTask -TaskName '{TASK_NAME}'
"#
    );
    let params = format!(
        "-NoProfile -NonInteractive -WindowStyle Hidden -ExecutionPolicy Bypass -EncodedCommand {}",
        encode_ps(&script)
    );
    let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    let (verb, file, args) = (wide("runas"), wide("powershell.exe"), wide(&params));
    unsafe {
        let mut info = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOCLOSEPROCESS,
            lpVerb: windows::core::PCWSTR(verb.as_ptr()),
            lpFile: windows::core::PCWSTR(file.as_ptr()),
            lpParameters: windows::core::PCWSTR(args.as_ptr()),
            nShow: 0,
            ..Default::default()
        };
        ShellExecuteExW(&mut info)
            .map_err(|_| "Administrator permission was declined".to_string())?;
        if info.hProcess.is_invalid() {
            return Err("Couldn't start the setup".into());
        }
        WaitForSingleObject(info.hProcess, if install { 600_000 } else { 60_000 });
        let mut code = 1u32;
        let _ = GetExitCodeProcess(info.hProcess, &mut code);
        let _ = CloseHandle(info.hProcess);
        match code {
            0 => {}
            3 => {
                return Err(
                    "winget isn't available. Install HWiNFO from hwinfo.com, then try again".into(),
                );
            }
            4 => {
                return Err(
                    "HWiNFO couldn't be installed. Check your connection and try again".into(),
                );
            }
            _ => return Err(format!("Setup failed (exit code {code})")),
        }
    }
    Ok(())
}

/// Cheap status check for the settings UI (doesn't start anything).
pub fn quick_state() -> (SensorState, &'static str) {
    if let HwinfoSnapshot::Ok(_) = read_hwinfo() {
        return (SensorState::Live, "HWiNFO");
    }
    for (ns, label) in [
        ("root\\LibreHardwareMonitor", "LibreHardwareMonitor"),
        ("root\\OpenHardwareMonitor", "OpenHardwareMonitor"),
    ] {
        if let Ok(c) = wmi::WMIConnection::with_namespace_path(ns)
            && c.raw_query::<Row>("SELECT Name FROM Sensor")
                .is_ok_and(|r| !r.is_empty())
        {
            return (SensorState::Live, label);
        }
    }
    if hwinfo_exe().is_none() {
        return (SensorState::NoProvider, "");
    }
    let state = if task_installed() {
        SensorState::HwinfoStarting
    } else if hwinfo_running() {
        SensorState::HwinfoNoSharedMemory
    } else {
        SensorState::HwinfoNeedsSetup
    };
    (state, "")
}

// Provider: picks the best available source and keeps HWiNFO alive when allowed to.

pub struct Sensors {
    lhm: Option<(wmi::WMIConnection, &'static str)>,
    last_lhm_probe: Option<Instant>,
    last_env_check: Option<Instant>,
    hwinfo_installed: bool,
    task: bool,
    last_task_run: Option<Instant>,
    sm_bad_since: Option<Instant>,
    pub state: SensorState,
}

impl Sensors {
    pub fn new() -> Self {
        Self {
            lhm: None,
            last_lhm_probe: None,
            last_env_check: None,
            hwinfo_installed: false,
            task: false,
            last_task_run: None,
            sm_bad_since: None,
            state: SensorState::Unknown,
        }
    }

    /// Forces a re-check of installation / task state (after setup).
    pub fn refresh_env(&mut self) {
        self.last_env_check = None;
    }

    pub fn sample(&mut self) -> SensorReading {
        if self
            .last_env_check
            .is_none_or(|t| t.elapsed() > Duration::from_secs(20))
        {
            self.last_env_check = Some(Instant::now());
            self.hwinfo_installed = hwinfo_exe().is_some();
            self.task = self.hwinfo_installed && task_installed();
        }

        match read_hwinfo() {
            HwinfoSnapshot::Ok(rs) => {
                self.sm_bad_since = None;
                self.state = SensorState::Live;
                pick_hwinfo(&rs)
            }
            snap => {
                let since = *self.sm_bad_since.get_or_insert_with(Instant::now);
                let running = self.hwinfo_installed && hwinfo_running();
                // Auto-manage HWiNFO once the user has set it up: start it when it's not running,
                // restart it when shared memory went inactive (free version: every 12 h).
                if self.task {
                    let should_start = !running
                        || (!matches!(snap, HwinfoSnapshot::Missing)
                            || since.elapsed() > Duration::from_secs(25));
                    let throttle = if running {
                        Duration::from_secs(600)
                    } else {
                        Duration::from_secs(90)
                    };
                    if should_start && self.last_task_run.is_none_or(|t| t.elapsed() > throttle) {
                        self.last_task_run = Some(Instant::now());
                        run_task();
                    }
                }
                let lhm = self.sample_lhm();
                if lhm.source.is_empty() {
                    self.state = if self.task
                        && self
                            .last_task_run
                            .is_some_and(|t| t.elapsed() < Duration::from_secs(40))
                    {
                        SensorState::HwinfoStarting
                    } else if running && !self.task {
                        SensorState::HwinfoNoSharedMemory
                    } else if self.hwinfo_installed && !self.task {
                        SensorState::HwinfoNeedsSetup
                    } else if self.task {
                        SensorState::HwinfoStarting
                    } else {
                        SensorState::NoProvider
                    };
                } else {
                    self.state = SensorState::Live;
                }
                lhm
            }
        }
    }

    fn sample_lhm(&mut self) -> SensorReading {
        if self.lhm.is_none()
            && self
                .last_lhm_probe
                .is_none_or(|t| t.elapsed() > Duration::from_secs(30))
        {
            self.last_lhm_probe = Some(Instant::now());
            for (ns, label) in [
                ("root\\LibreHardwareMonitor", "LibreHardwareMonitor"),
                ("root\\OpenHardwareMonitor", "OpenHardwareMonitor"),
            ] {
                if let Ok(c) = wmi::WMIConnection::with_namespace_path(ns)
                    && c.raw_query::<Row>("SELECT Name FROM Sensor")
                        .is_ok_and(|r| !r.is_empty())
                {
                    self.lhm = Some((c, label));
                    break;
                }
            }
        }
        if let Some((c, label)) = &self.lhm {
            match c.raw_query::<Row>(
                "SELECT Name, SensorType, Value, Identifier FROM Sensor WHERE SensorType='Temperature' OR SensorType='Fan' OR SensorType='Load' OR SensorType='Power'",
            ) {
                Ok(rows) => return pick_lhm(&rows, label),
                Err(_) => self.lhm = None,
            }
        }
        SensorReading::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put_str(b: &mut [u8], o: usize, s: &str) {
        b[o..o + s.len()].copy_from_slice(s.as_bytes());
    }

    /// Builds a synthetic HWiNFO block with the documented layout and parses it back.
    #[test]
    fn hwinfo_layout() {
        let (s_size, r_size) = (264usize, 316usize);
        let sensors = [
            "CPU [#0]: AMD Ryzen 7 5800X: Enhanced",
            "Nuvoton NCT6798D",
            "GPU [#0]: NVIDIA GeForce RTX 5070",
        ];
        let readings: [(u32, u32, &str, f64, f64); 7] = [
            (1, 0, "CPU (Tctl/Tdie)", 54.5, 70.0),
            (1, 0, "CPU Core", 40.0, 60.0),
            (5, 0, "CPU Package Power", 61.2, 90.0),
            (6, 0, "Average Effective Clock", 3820.0, 4800.0),
            (3, 1, "CPU", 1180.0, 1400.0),
            (3, 1, "Chassis2", 0.0, 0.0),
            (3, 2, "GPU Fan1", 0.0, 1100.0),
        ];
        let s_off = 44;
        let r_off = s_off + s_size * sensors.len();
        let mut b = vec![0u8; r_off + r_size * readings.len()];
        b[0..4].copy_from_slice(b"HWiS");
        b[12..20].copy_from_slice(&1000i64.to_le_bytes());
        for (o, v) in [
            (20, s_off),
            (24, s_size),
            (28, sensors.len()),
            (32, r_off),
            (36, r_size),
            (40, readings.len()),
        ] {
            b[o..o + 4].copy_from_slice(&(v as u32).to_le_bytes());
        }
        for (i, n) in sensors.iter().enumerate() {
            put_str(&mut b, s_off + i * s_size + 8, n);
        }
        for (i, (t, si, l, v, mx)) in readings.iter().enumerate() {
            let o = r_off + i * r_size;
            b[o..o + 4].copy_from_slice(&t.to_le_bytes());
            b[o + 4..o + 8].copy_from_slice(&si.to_le_bytes());
            put_str(&mut b, o + 12, l);
            b[o + 284..o + 292].copy_from_slice(&v.to_le_bytes());
            b[o + 300..o + 308].copy_from_slice(&mx.to_le_bytes());
        }
        let HwinfoSnapshot::Ok(rs) = parse_hwinfo(&b, 1005) else {
            panic!("parse failed")
        };
        let r = pick_hwinfo(&rs);
        assert_eq!(r.cpu_temp, Some(54.5));
        assert_eq!(r.cpu_power, Some(61.2));
        assert_eq!(r.cpu_clock, Some(3820.0));
        assert_eq!(
            r.fans,
            vec![
                Fan {
                    name: "CPU".into(),
                    rpm: 1180.0
                },
                Fan {
                    name: "GPU Fan1".into(),
                    rpm: 0.0
                }
            ]
        );
        assert!(matches!(parse_hwinfo(&b, 2000), HwinfoSnapshot::Stale));
        b[0..4].copy_from_slice(b"DEAD");
        assert!(matches!(parse_hwinfo(&b, 1005), HwinfoSnapshot::Inactive));
    }

    #[test]
    fn b64() {
        assert_eq!(base64(b"Man"), "TWFu");
        assert_eq!(base64(b"Ma"), "TWE=");
        assert_eq!(base64(b"M"), "TQ==");
    }
}
