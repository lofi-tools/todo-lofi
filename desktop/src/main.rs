#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    let todo_api = todo_core::AppState::new();

    tauri::Builder::default()
        .manage(todo_api)
        .setup(|_app| Ok(()))
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
