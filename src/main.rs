#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod ascii;
mod codec;
mod link;
mod modbus;
mod project;
mod rest;
mod script;
mod sessions;
mod terminal;
mod tools;

use std::path::PathBuf;

use eframe::egui;

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let shot = args.iter().position(|a| a == "--screenshot").map(|i| {
        PathBuf::from(args.get(i + 1).cloned().unwrap_or_else(|| "screenshot.png".into()))
    });
    // a project file given on the command line (double click on a .portwerk file)
    let mut open_file = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--screenshot" | "--tab" | "--mode" => i += 2,
            a if a.starts_with("--") => i += 1,
            a => {
                open_file = Some(PathBuf::from(a));
                i += 1;
            }
        }
    }

    let mut viewport = egui::ViewportBuilder::default()
        .with_title(format!("{} {}", app::APP_NAME, app::APP_VERSION))
        .with_inner_size([1180.0, 760.0])
        .with_min_inner_size([820.0, 520.0]);

    #[allow(unused_mut)]
    let mut options = eframe::NativeOptions::default();
    if shot.is_some() {
        viewport = viewport.with_active(false);
        #[cfg(windows)]
        {
            viewport = viewport.with_window_level(egui::WindowLevel::AlwaysOnBottom);
        }
        #[cfg(target_os = "macos")]
        {
            options.event_loop_builder = Some(Box::new(|b| {
                use winit::platform::macos::EventLoopBuilderExtMacOS;
                b.with_activate_ignoring_other_apps(false);
            }));
        }
        options.persist_window = false;
    }
    options.viewport = viewport;

    eframe::run_native(
        app::APP_NAME,
        options,
        Box::new(move |cc| {
            let mut a = app::PortWerkApp::new(cc, shot, open_file);
            // optional: --tab <serial|tcpc|tcps|udp|modbus|rest|tools|info> [--mode lines|loop|script] [--ascii]
            let get = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
            a.apply_start_args(get("--tab").as_deref(), get("--mode").as_deref(), args.iter().any(|a| a == "--ascii"));
            Ok(Box::new(a))
        }),
    )
}
