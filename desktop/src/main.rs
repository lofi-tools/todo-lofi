#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            // Initialize core library
            core::init();
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
