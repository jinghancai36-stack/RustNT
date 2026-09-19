use std::io::{self, Write};
use std::thread;
use std::time::Duration;

fn startup_line(pid: u32) -> String {
    format!("RUSTNT_TASK09_TARGET_PID={pid}\n")
}

fn main() {
    print!("{}", startup_line(std::process::id()));
    io::stdout().flush().expect("startup line must be flushed");
    loop {
        thread::sleep(Duration::from_secs(60));
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn startup_line_contains_the_target_pid() {
        assert_eq!(super::startup_line(1234), "RUSTNT_TASK09_TARGET_PID=1234\n");
    }
}
