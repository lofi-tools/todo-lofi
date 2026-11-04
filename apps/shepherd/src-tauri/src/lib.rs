//! Shepherd's desktop shell.
//!
//! This crate owns the Tauri window, the invoke commands and the event channel.
//! Task behaviour lives in `shepherd-core`; the shell never holds a second copy
//! of the state model.

/// Build and run the window. A startup failure is returned so `main` can report
/// it and exit with a non-zero status.
pub fn run() -> Result<(), tauri::Error> {
    tauri::Builder::default().run(tauri::generate_context!())
}
