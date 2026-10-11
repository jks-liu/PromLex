fn main() {
    build_support::relocate_tauri_metadata().expect("unable to relocate cached Tauri permissions");
    tauri_build::build()
}

mod build_support;
