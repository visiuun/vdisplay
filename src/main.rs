#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![allow(clippy::too_many_arguments, clippy::chunks_exact_to_as_chunks)]
//! Vdisplay: controller for the MARS GAMING MC-3TCORELCD (DS916) 360×960 case screen.

mod device;
mod engine;
mod gui;
mod hw;
mod loader;
mod lyrics;
mod media;
mod net;
mod render;
mod sensors;
mod settings;
mod shared;
mod winutil;

use std::sync::Arc;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--render-test") {
        let dir = args
            .get(i + 1)
            .cloned()
            .unwrap_or_else(|| "render-test".into());
        render_test(std::path::Path::new(&dir));
        return;
    }

    // A second launch just brings the running instance to the front.
    let Some(instance) = winutil::SingleInstance::acquire() else {
        return;
    };
    let instance = Arc::new(instance);

    winutil::begin_timer_resolution();
    let settings = settings::Settings::load();
    let start_visible = !(settings.start_minimized || args.iter().any(|a| a == "--minimized"));
    let (media_src, media_fit) = (settings.media_path.clone(), settings.media_fit);

    let sh = shared::Shared::new(settings, loader::load_preset("gradient"));
    sh.start_saver();
    if media::demo_enabled() {
        let t = media::demo_track(0.0);
        if let Some(l) = lyrics::parse_lrc(media::DEMO_LRC) {
            sh.lyrics.seed(&t.title, &t.artist, l);
        }
    }
    if !media_src.starts_with("preset:gradient") {
        // Large GIFs take a moment to decode; never block startup on them.
        let sh2 = sh.clone();
        std::thread::spawn(move || match loader::load_source(&media_src, media_fit) {
            Ok(m) => sh2.set_media(m),
            Err(e) => {
                *sh2.media_message.lock() = format!("Saved media unavailable ({e}), using preset")
            }
        });
    }
    let engine = engine::Engine::spawn(sh.clone());

    // Keep HWiNFO (sensor backend) out of the notification area.
    let sh_tray = sh.clone();
    std::thread::spawn(move || {
        while !sh_tray.quit.load(std::sync::atomic::Ordering::Acquire) {
            winutil::hide_hwinfo_tray_icon();
            std::thread::sleep(std::time::Duration::from_secs(2));
        }
    });

    let (tx, rx) = std::sync::mpsc::channel();
    let icon = eframe::egui::IconData {
        rgba: render::icons::logo_rgba(64),
        width: 64,
        height: 64,
    };
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Vdisplay")
            .with_inner_size([1200.0, 820.0])
            .with_min_inner_size([780.0, 640.0])
            .with_icon(Arc::new(icon))
            .with_visible(start_visible),
        ..Default::default()
    };
    let sh_exit = sh.clone();
    let _ = eframe::run_native(
        "Vdisplay",
        options,
        Box::new(move |cc| {
            Ok(Box::new(gui::App::new(
                cc,
                sh,
                engine,
                start_visible,
                tx,
                rx,
                Some(instance),
            )))
        }),
    );
    sh_exit
        .quit
        .store(true, std::sync::atomic::Ordering::Release);
    sh_exit.save_now();
    winutil::end_timer_resolution();
}

/// Developer aid: renders every screen to PNG files (`vdisplay --render-test <dir>`).
fn render_test(dir: &std::path::Path) {
    use render::{MusicOpts, Renderer};
    let _ = std::fs::create_dir_all(dir);
    let mut r = Renderer::new();
    let save = |r: &Renderer, name: &str| {
        let mut rgb = Vec::new();
        r.canvas.to_rgb(&mut rgb);
        let img = image::RgbImage::from_raw(render::W, render::H, rgb).unwrap();
        let _ = img.save(dir.join(format!("{name}.png")));
    };

    let info = media::demo_track(61_500.0);
    let store = lyrics::LyricsStore::new((String::new(), String::new()));
    let opts = MusicOpts {
        bg_style: settings::BgStyle::Radiant,
        show_lyrics: false,
        lyrics_offset_ms: 0.0,
    };
    r.music(Some(&info), info.position_ms, &opts, &store);
    save(&r, "music");
    let g = MusicOpts {
        bg_style: settings::BgStyle::Gradient,
        ..opts
    };
    r.music(Some(&info), info.position_ms, &g, &store);
    save(&r, "music_gradient");

    let lrc = media::DEMO_LRC;
    let ly = lyrics::parse_lrc(lrc).unwrap();
    store.seed(&info.title, &info.artist, ly);
    let lo = MusicOpts {
        show_lyrics: true,
        ..opts
    };
    r.music(Some(&info), 61_600.0, &lo, &store);
    save(&r, "lyrics");

    r.music(None, 0.0, &lo, &store);
    save(&r, "standby");

    let fan = |n: &str, rpm: f32| sensors::Fan {
        name: n.into(),
        rpm,
    };
    let stats = hw::HwStats {
        valid: true,
        cpu_name: "AMD Ryzen 7 5800X".into(),
        cpu_pct: 37.0,
        cpu_temp: Some(61.0),
        cpu_power_w: Some(72.4),
        cpu_clock_mhz: Some(4412.0),
        gpu: Some(hw::GpuStats {
            name: "RTX 5070".into(),
            util: 72.0,
            temp: Some(79.0),
            vram_used_gb: 6.3,
            vram_total_gb: 11.9,
            fan_rpm: Some(1420.0),
            power_w: Some(188.0),
            clock_mhz: Some(2655.0),
        }),
        ram_pct: 48.0,
        ram_used_gb: 15.4,
        ram_total_gb: 31.9,
        disk_pct: 84.0,
        net_down_kbs: 2480.0,
        net_up_kbs: 96.0,
        top: vec![
            ("TIDAL".into(), 4.2, 1658),
            ("SignalRgb".into(), 2.3, 293),
            ("Discord".into(), 1.8, 2063),
        ],
        fans: vec![
            fan("CPU", 1180.0),
            fan("Pump", 2650.0),
            fan("Chassis1", 860.0),
            fan("Chassis2", 0.0),
            fan("GPU Fan1", 1420.0),
        ],
        sensor_source: "HWiNFO",
        sensor_state: sensors::SensorState::Live,
    };
    r.hardware(&stats);
    save(&r, "hardware");
    let bare = hw::HwStats {
        cpu_temp: None,
        cpu_power_w: None,
        cpu_clock_mhz: None,
        fans: vec![fan("GPU", 0.0)],
        sensor_source: "",
        sensor_state: sensors::SensorState::HwinfoNeedsSetup,
        ..stats.clone()
    };
    r.hardware(&bare);
    save(&r, "hardware_nosensors");
    r.startup_test("COM5", 60);
    save(&r, "test");
    r.fullscreen_paused("Cyberpunk2077.exe");
    save(&r, "paused");
    r.music(Some(&info), info.position_ms, &opts, &store);
    r.debug_overlay(&render::DebugHud {
        fps: 59.8,
        kb: 24.1,
        mbps: 11.5,
        rnd_ms: 1.9,
        jpg_ms: 1.4,
        tx_ms: 0.9,
        tag: "WORD SYNC".into(),
    });
    save(&r, "debug");
    let p = r.preset("gradient");
    let _ = p.save(dir.join("preset.png"));
    let _ = std::fs::write(dir.join("vdisplay.ico"), build_ico());

    // Timing breakdown (averages over 120 frames).
    let n = 120;
    let mut rgb = Vec::new();
    let mut jpeg = Vec::new();
    let mut report = String::new();
    let mut bench = |label: &str, r: &mut Renderer, f: &dyn Fn(&mut Renderer, usize)| {
        let t = std::time::Instant::now();
        for i in 0..n {
            f(r, i);
        }
        let render_ms = t.elapsed().as_secs_f64() * 1000.0 / n as f64;
        let t = std::time::Instant::now();
        for _ in 0..n {
            r.canvas.to_rgb(&mut rgb);
            jpeg.clear();
            let mut enc = jpeg_encoder::Encoder::new(&mut jpeg, 76);
            enc.set_sampling_factor(jpeg_encoder::SamplingFactor::F_2_2);
            enc.encode(
                &rgb,
                render::W as u16,
                render::H as u16,
                jpeg_encoder::ColorType::Rgb,
            )
            .unwrap();
        }
        let jpeg_ms = t.elapsed().as_secs_f64() * 1000.0 / n as f64;
        report += &format!(
            "{label:<12} render {render_ms:5.2} ms | jpeg {jpeg_ms:5.2} ms | {} KB
",
            jpeg.len() / 1024
        );
    };
    bench("now playing", &mut r, &|r, i| {
        r.music(
            Some(&info),
            info.position_ms + i as f64 * 16.0,
            &opts,
            &store,
        )
    });
    bench("lyrics", &mut r, &|r, i| {
        r.music(Some(&info), 61_600.0 + i as f64 * 16.0, &lo, &store)
    });
    bench("hardware", &mut r, &|r, _| r.hardware(&stats));
    bench("standby", &mut r, &|r, _| r.standby());
    let _ = std::fs::write(dir.join("timing.txt"), report);
}

/// Multi-resolution .ico (PNG-compressed entries) of the app logo, for embedding in the exe.
fn build_ico() -> Vec<u8> {
    let sizes = [16u32, 20, 24, 32, 40, 48, 64, 128, 256];
    let pngs: Vec<Vec<u8>> = sizes
        .iter()
        .map(|&s| {
            let img = image::RgbaImage::from_raw(s, s, render::icons::logo_rgba(s)).unwrap();
            let mut buf = std::io::Cursor::new(Vec::new());
            img.write_to(&mut buf, image::ImageFormat::Png).unwrap();
            buf.into_inner()
        })
        .collect();
    let mut out = vec![0, 0, 1, 0];
    out.extend_from_slice(&(sizes.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * sizes.len() as u32;
    for (s, png) in sizes.iter().zip(&pngs) {
        let d = if *s >= 256 { 0 } else { *s as u8 };
        out.extend_from_slice(&[d, d, 0, 0, 1, 0, 32, 0]);
        out.extend_from_slice(&(png.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += png.len() as u32;
    }
    for png in pngs {
        out.extend_from_slice(&png);
    }
    out
}
