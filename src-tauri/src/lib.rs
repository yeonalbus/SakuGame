// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
mod commands;
mod db;
mod error;
mod locator;
mod models;
mod scanner;
mod state;

use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            let db = db::Db::open(&data_dir.join("sakugame.db"))?;
            app.manage(state::AppState::new(db));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_repositories,
            commands::check_repository,
            commands::add_repository,
            commands::update_repository,
            commands::delete_repository,
            commands::list_games,
            commands::get_game,
            commands::update_game,
            commands::start_scan,
            commands::pause_scan,
            commands::resume_scan,
            commands::cancel_scan,
            commands::scan_status,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
