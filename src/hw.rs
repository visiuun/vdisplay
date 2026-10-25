//! PC telemetry sampler (CPU / GPU / RAM / disk / network / sensors / top processes).
//!
//! Only runs while a hardware screen is actually being shown. Values are never fabricated:
//! temperatures / fans / power come from a sensor provider (see `sensors`), GPU data from NVML,
//! and anything unavailable is reported as `None`.

use crate::sensors::{Fan, SensorState, Sensors};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use sysinfo::{
    CpuRefreshKind, Disks, MemoryRefreshKind, Networks, ProcessRefreshKind, ProcessesToUpdate,
    RefreshKind, System,
};

#[derive(Clone, Default)]
pub struct GpuStats {
    pub name: String,
    pub util: f32,
    pub temp: Option<f32>,
    pub vram_used_gb: f32,
    pub vram_total_gb: f32,
    pub fan_rpm: Option<f32>,
    pub power_w: Option<f32>,
    pub clock_mhz: Option<f32>,
}

#[derive(Clone, Default)]
pub struct HwStats {
    pub valid: bool,
    pub cpu_name: String,
    pub cpu_pct: f32,
    pub cpu_temp: Option<f32>,
    pub cpu_power_w: Option<f32>,
    pub cpu_clock_mhz: Option<f32>,
    pub gpu: Option<GpuStats>,
    pub ram_pct: f32,
    pub ram_used_gb: f32,
    pub ram_total_gb: f32,
    pub disk_pct: f32,
    pub net_down_kbs: f32,
    pub net_up_kbs: f32,
    /// (name, cpu % of whole machine, memory MB)
    pub top: Vec<(String, f32, u64)>,
    pub fans: Vec<Fan>,
    pub sensor_source: &'static str,
    pub sensor_state: SensorState,
}

pub struct HwSampler {
    stats: Mutex<HwStats>,
    pub enabled: AtomicBool,
    /// Set after the HWiNFO setup so the provider re-checks installation state immediately.
    pub env_changed: AtomicBool,
}

impl HwSampler {
    pub fn start() -> Arc<Self> {
        let me = Arc::new(Self {
            stats: Mutex::new(HwStats::default()),
            enabled: AtomicBool::new(false),
            env_changed: AtomicBool::new(false),
        });
        let m = me.clone();
        std::thread::Builder::new()
            .name("hw-sampler".into())
            .spawn(move || m.run())
            .ok();
        me
    }

    pub fn stats(&self) -> HwStats {
        self.stats.lock().clone()
    }

    fn run(self: Arc<Self>) {
        let mut sys = System::new_with_specifics(
            RefreshKind::nothing()
                .with_cpu(CpuRefreshKind::nothing().with_cpu_usage())
                .with_memory(MemoryRefreshKind::everything()),
        );
        let mut disks = Disks::new_with_refreshed_list();
        let mut nets = Networks::new_with_refreshed_list();
        let mut last_net: Option<(u64, u64, Instant)> = None;
        let mut last_procs = Instant::now() - Duration::from_secs(60);
        let mut gpu = Gpu::new();
        let mut sensors = Sensors::new();
        let cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1) as f32;

        loop {
            if !self.enabled.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(400));
                continue;
            }
            if self.env_changed.swap(false, Ordering::AcqRel) {
                sensors.refresh_env();
            }
            let mut s = self.stats.lock().clone();
            s.valid = true;

            sys.refresh_cpu_usage();
            sys.refresh_memory();
            s.cpu_pct = sys.global_cpu_usage().clamp(0.0, 100.0);
            if s.cpu_name.is_empty() {
                sys.refresh_cpu_specifics(CpuRefreshKind::nothing().with_frequency());
                s.cpu_name = sys
                    .cpus()
                    .first()
                    .map(|c| short_cpu_name(c.brand()))
                    .unwrap_or_default();
            }
            let gb = 1024.0f32.powi(3);
            s.ram_total_gb = sys.total_memory() as f32 / gb;
            s.ram_used_gb = sys.used_memory() as f32 / gb;
            s.ram_pct = if s.ram_total_gb > 0.0 {
                s.ram_used_gb / s.ram_total_gb * 100.0
            } else {
                0.0
            };

            disks.refresh(false);
            if let Some(d) = disks.list().iter().find(|d| {
                d.mount_point()
                    .to_string_lossy()
                    .to_uppercase()
                    .starts_with("C:")
            }) {
                let t = d.total_space() as f32;
                if t > 0.0 {
                    s.disk_pct = (t - d.available_space() as f32) / t * 100.0;
                }
            }

            nets.refresh(true);
            let (rx, tx) = nets.values().fold((0u64, 0u64), |a, n| {
                (a.0 + n.total_received(), a.1 + n.total_transmitted())
            });
            if let Some((prx, ptx, pt)) = last_net {
                let dt = pt.elapsed().as_secs_f32().max(0.1);
                s.net_down_kbs = rx.saturating_sub(prx) as f32 / 1024.0 / dt;
                s.net_up_kbs = tx.saturating_sub(ptx) as f32 / 1024.0 / dt;
            }
            last_net = Some((rx, tx, Instant::now()));

            if last_procs.elapsed() >= Duration::from_secs(3) {
                last_procs = Instant::now();
                // A persistent System keeps per-process CPU deltas between refreshes.
                sys.refresh_processes_specifics(
                    ProcessesToUpdate::All,
                    true,
                    ProcessRefreshKind::nothing().with_cpu().with_memory(),
                );
                let mut agg: HashMap<String, (f32, u64)> = HashMap::new();
                for p in sys.processes().values() {
                    let name = p.name().to_string_lossy().into_owned();
                    let lower = name.to_lowercase();
                    if lower.is_empty()
                        || matches!(
                            lower.as_str(),
                            "system idle process"
                                | "idle"
                                | "system"
                                | "memory compression"
                                | "registry"
                        )
                    {
                        continue;
                    }
                    let e = agg.entry(name).or_default();
                    e.0 += p.cpu_usage() / cores;
                    e.1 += p.memory() / (1024 * 1024);
                }
                let mut list: Vec<_> = agg
                    .into_iter()
                    .map(|(n, (c, m))| (n.trim_end_matches(".exe").to_string(), c, m))
                    .collect();
                list.sort_by(|a, b| b.1.total_cmp(&a.1).then(b.2.cmp(&a.2)));
                list.truncate(4);
                s.top = list;
            }

            s.gpu = gpu.sample();

            let r = sensors.sample();
            s.sensor_state = sensors.state;
            s.sensor_source = r.source;
            s.cpu_temp = r.cpu_temp;
            s.cpu_power_w = r.cpu_power;
            s.cpu_clock_mhz = r.cpu_clock;
            s.fans = r.fans;
            match &mut s.gpu {
                Some(g) => {
                    g.temp = g.temp.or(r.gpu_temp);
                    // NVML knows the GPU fan even without a sensor app.
                    let has_gpu_fan = s
                        .fans
                        .iter()
                        .any(|f| f.name.to_lowercase().starts_with("gpu"));
                    if let Some(rpm) = g.fan_rpm.filter(|_| !has_gpu_fan) {
                        s.fans.push(Fan {
                            name: "GPU".into(),
                            rpm,
                        });
                    }
                }
                None if r.gpu_load.is_some() => {
                    s.gpu = Some(GpuStats {
                        name: "GPU".into(),
                        util: r.gpu_load.unwrap_or(0.0),
                        temp: r.gpu_temp,
                        ..Default::default()
                    });
                }
                None => {}
            }

            *self.stats.lock() = s;
            std::thread::sleep(Duration::from_millis(1000));
        }
    }
}

fn short_cpu_name(brand: &str) -> String {
    let mut s = brand
        .replace("(R)", "")
        .replace("(TM)", "")
        .replace("CPU", "");
    for junk in [
        " Processor",
        " with Radeon Graphics",
        " 8-Core",
        " 6-Core",
        " 12-Core",
        " 16-Core",
    ] {
        s = s.replace(junk, "");
    }
    if let Some(i) = s.find('@') {
        s.truncate(i);
    }
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

// NVIDIA via NVML (no nvidia-smi process spawning)

struct Gpu {
    nvml: Option<nvml_wrapper::Nvml>,
    tried: bool,
}

impl Gpu {
    fn new() -> Self {
        Self {
            nvml: None,
            tried: false,
        }
    }

    fn sample(&mut self) -> Option<GpuStats> {
        use nvml_wrapper::enum_wrappers::device::{Clock, TemperatureSensor};
        if !self.tried {
            self.tried = true;
            self.nvml = nvml_wrapper::Nvml::init().ok();
        }
        let nvml = self.nvml.as_ref()?;
        let dev = nvml.device_by_index(0).ok()?;
        let name = dev
            .name()
            .unwrap_or_else(|_| "GPU".into())
            .replace("NVIDIA GeForce ", "")
            .replace("NVIDIA ", "");
        let (used, total) = dev
            .memory_info()
            .map(|m| (m.used, m.total))
            .unwrap_or((0, 0));
        let gb = 1024.0f32.powi(3);
        Some(GpuStats {
            name,
            util: dev.utilization_rates().map(|u| u.gpu as f32).unwrap_or(0.0),
            temp: dev
                .temperature(TemperatureSensor::Gpu)
                .ok()
                .map(|t| t as f32),
            vram_used_gb: used as f32 / gb,
            vram_total_gb: total as f32 / gb,
            fan_rpm: dev.fan_speed_rpm(0).ok().map(|f| f as f32),
            power_w: dev.power_usage().ok().map(|mw| mw as f32 / 1000.0),
            clock_mhz: dev.clock_info(Clock::Graphics).ok().map(|c| c as f32),
        })
    }
}

#[cfg(test)]
mod tests {
    /// Live sampler check: `cargo test --release -- --ignored --nocapture hw_live`
    #[test]
    #[ignore]
    fn hw_live() {
        let s = super::HwSampler::start();
        s.enabled.store(true, std::sync::atomic::Ordering::Relaxed);
        std::thread::sleep(std::time::Duration::from_millis(4500));
        let st = s.stats();
        println!(
            "cpu {} {:.0}% temp {:?} power {:?} clock {:?} src '{}' state {:?}",
            st.cpu_name,
            st.cpu_pct,
            st.cpu_temp,
            st.cpu_power_w,
            st.cpu_clock_mhz,
            st.sensor_source,
            st.sensor_state
        );
        println!("fans {:?}", st.fans);
        if let Some(g) = &st.gpu {
            println!(
                "gpu {} {:.0}% temp {:?} vram {:.1}/{:.1} fan {:?} rpm power {:?} clock {:?}",
                g.name,
                g.util,
                g.temp,
                g.vram_used_gb,
                g.vram_total_gb,
                g.fan_rpm,
                g.power_w,
                g.clock_mhz
            );
        }
        println!(
            "ram {:.1}/{:.1} GB ({:.0}%) disk {:.0}% net {:.0}/{:.0} KB/s",
            st.ram_used_gb,
            st.ram_total_gb,
            st.ram_pct,
            st.disk_pct,
            st.net_down_kbs,
            st.net_up_kbs
        );
        println!("top {:?}", st.top);
        assert!(st.valid && st.ram_total_gb > 0.0);
    }
}
