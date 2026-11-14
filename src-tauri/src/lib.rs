// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
pub mod cache;
pub mod commands;
pub mod db;
pub mod error;
pub mod locator;
pub mod models;
pub mod scanner;
pub mod state;
pub mod store;
pub mod transfer;

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
            // v2：启动对账（缓存记录 vs 池内目录）+ 未完成任务转暂停
            let _ = cache::reconcile(&db);
            let _ = transfer::recover_on_startup(&db);
            let state = state::AppState::new(db);
            let engine = state.transfer.clone();
            app.manage(state);
            transfer::spawn_scheduler(engine, app.handle().clone());
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
            commands::update_version,
            commands::set_selected_version,
            commands::set_selected_executable,
            commands::list_executable_candidates,
            commands::create_manual_version,
            commands::add_manual_executable,
            commands::delete_version,
            commands::delete_game,
            commands::list_blocklist,
            commands::unblock,
            commands::start_scan,
            commands::scan_single_game,
            commands::pause_scan,
            commands::resume_scan,
            commands::cancel_scan,
            commands::scan_status,
            commands::get_cache_config,
            commands::set_cache_config,
            commands::get_cache_usage,
            commands::preflight_transfer,
            commands::start_transfer,
            commands::list_transfers,
            commands::get_version_transfer,
            commands::pause_transfer,
            commands::resume_transfer,
            commands::cancel_transfer,
            commands::retry_transfer,
            commands::remove_cache,
            commands::get_version_states,
            commands::launch_game,
            commands::get_repository_delete_preview,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
