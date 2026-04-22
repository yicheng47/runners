mod commands;
mod db;
mod error;
mod event_bus;
mod event_log;
mod model;
mod orchestrator;
mod session;

use tauri::Manager;

pub struct AppState {
    pub db: db::DbPool,
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_shell::init())
        .setup(|app| {
            let app_data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&app_data_dir)?;
            let db_path = app_data_dir.join("runners.db");
            let pool = db::open_pool(&db_path)?;
            app.manage(AppState { db: pool });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::crew::crew_list,
            commands::crew::crew_get,
            commands::crew::crew_create,
            commands::crew::crew_update,
            commands::crew::crew_delete,
            commands::runner::runner_list,
            commands::runner::runner_get,
            commands::runner::runner_create,
            commands::runner::runner_update,
            commands::runner::runner_delete,
            commands::runner::runner_set_lead,
            commands::runner::runner_reorder,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
