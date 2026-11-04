#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if let Err(error) = shepherd_shell::run() {
        eprintln!("Shepherd failed to start: {error}");
        std::process::exit(1);
    }
}
