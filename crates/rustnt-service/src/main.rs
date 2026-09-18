#![cfg(windows)]

fn main() {
    if let Err(error) = rustnt_core::service::run_service() {
        eprintln!("rustnt-service error: {error}");
        std::process::exit(1);
    }
}
