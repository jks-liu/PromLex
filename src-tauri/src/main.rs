#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

fn main() {
    if std::env::args().any(|argument| argument == "--server") {
        promlex_lib::run_server();
    } else {
        promlex_lib::run();
    }
}
