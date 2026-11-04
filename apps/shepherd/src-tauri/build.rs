fn main() {
    if let Err(error) = tauri_build::try_build(tauri_build::Attributes::default()) {
        eprintln!("tauri build script failed: {error}");
        std::process::exit(1);
    }
}
