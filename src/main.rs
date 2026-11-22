#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod config;
mod critic;
mod draft;
mod engine;
mod evidence;
mod llm;
mod markdown;
mod model;
mod plan;
mod report;
mod secrets;
mod store;
mod synth;
mod theme;
#[cfg(target_os = "windows")]
mod win;

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if let Some(position) = args.iter().position(|arg| arg == "--plan-only") {
        let query = args.get(position + 1).cloned().unwrap_or_default();
        std::process::exit(plan::plan_only(&query));
    }
    if let Some(position) = args.iter().position(|arg| arg == "--collect-only") {
        let query = args.get(position + 1).cloned().unwrap_or_default();
        std::process::exit(plan::collect_only(&query));
    }
    if let Some(position) = args.iter().position(|arg| arg == "--selftest") {
        let topic = args
            .get(position + 1)
            .cloned()
            .unwrap_or_else(|| "adderall".to_string());
        std::process::exit(engine::selftest(&topic));
    }

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1180.0, 800.0])
            .with_min_inner_size([960.0, 640.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Reddit Research",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc)) as Box<dyn eframe::App>)),
    )
}
