#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    #[cfg(unix)]
    if let Some(result) = shepherd_shell::setup_terminal::dispatch_helper() {
        if let Err(error) = result {
            eprintln!("Shepherd environment: {error}");
            std::process::exit(1);
        }
        return;
    }
    if let Err(error) = shepherd_shell::run() {
        eprintln!("Shepherd failed to start: {error}");
        std::process::exit(1);
    }
}
