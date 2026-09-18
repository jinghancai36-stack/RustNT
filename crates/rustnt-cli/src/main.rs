#![cfg(windows)]

use std::cmp::Ordering;
use std::env;
use std::io::{self, Write};
use std::process::ExitCode;
use std::time::Duration;

use rustnt_core::ProcessInfo;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SortKey {
    Pid,
    Name,
    Cpu,
    Memory,
}

struct Options {
    watch_seconds: Option<u64>,
    filter: Option<String>,
    sort: SortKey,
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.len() < 2 || args[0] != "process" || args[1] != "list" {
        print_usage();
        return ExitCode::from(2);
    }

    let options = match parse_options(&args[2..]) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("usage error: {error}");
            print_usage();
            return ExitCode::from(2);
        }
    };

    if let Some(interval) = options.watch_seconds {
        match run_watch(&options, interval) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("error: {error}");
                ExitCode::from(1)
            }
        }
    } else {
        match rustnt_core::list_processes() {
            Ok(processes) => {
                let processes = filter_processes(processes, &options);
                println!("{}", render_processes(&processes, options.sort));
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("error: {error}");
                ExitCode::from(1)
            }
        }
    }
}

fn print_usage() {
    eprintln!(
        "usage: rustnt process list [--watch <seconds>] [--filter <text>] \
         [--sort <pid|name|cpu|memory>]"
    );
}

fn parse_options(args: &[String]) -> Result<Options, String> {
    let mut options = Options {
        watch_seconds: None,
        filter: None,
        sort: SortKey::Pid,
    };
    let mut sort_seen = false;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--watch" => {
                if options.watch_seconds.is_some() {
                    return Err("duplicate --watch".to_string());
                }
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--watch requires a positive integer".to_string())?;
                if value.starts_with("--") {
                    return Err("--watch requires a positive integer".to_string());
                }
                let seconds = value
                    .parse::<u64>()
                    .map_err(|_| "--watch requires a positive integer".to_string())?;
                if seconds == 0 {
                    return Err("--watch must be greater than zero".to_string());
                }
                options.watch_seconds = Some(seconds);
                index += 2;
            }
            "--filter" => {
                if options.filter.is_some() {
                    return Err("duplicate --filter".to_string());
                }
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--filter requires text".to_string())?;
                if value.starts_with("--") {
                    return Err("--filter requires text".to_string());
                }
                options.filter = Some(value.clone());
                index += 2;
            }
            "--sort" => {
                if sort_seen {
                    return Err("duplicate --sort".to_string());
                }
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--sort requires pid, name, cpu, or memory".to_string())?;
                if value.starts_with("--") {
                    return Err("--sort requires pid, name, cpu, or memory".to_string());
                }
                options.sort = match value.as_str() {
                    "pid" => SortKey::Pid,
                    "name" => SortKey::Name,
                    "cpu" => SortKey::Cpu,
                    "memory" => SortKey::Memory,
                    _ => return Err(format!("invalid sort key: {value}")),
                };
                sort_seen = true;
                index += 2;
            }
            flag => return Err(format!("unknown option: {flag}")),
        }
    }

    Ok(options)
}

fn filter_processes(processes: Vec<ProcessInfo>, options: &Options) -> Vec<ProcessInfo> {
    processes
        .into_iter()
        .filter(|process| matches_filter(process, options.filter.as_deref()))
        .collect()
}

fn matches_filter(process: &ProcessInfo, filter: Option<&str>) -> bool {
    let Some(filter) = filter else {
        return true;
    };
    let filter = filter.to_lowercase();
    process.name.to_lowercase().contains(&filter)
        || process
            .path
            .as_deref()
            .unwrap_or_default()
            .to_lowercase()
            .contains(&filter)
}

fn sort_processes(processes: &mut [ProcessInfo], sort: SortKey) {
    processes.sort_by(|left, right| {
        let ordering = match sort {
            SortKey::Pid => left.pid.cmp(&right.pid),
            SortKey::Name => left.name.to_lowercase().cmp(&right.name.to_lowercase()),
            SortKey::Cpu => compare_optional_f32(left.cpu_percent, right.cpu_percent),
            SortKey::Memory => compare_optional_u64(left.memory_bytes, right.memory_bytes),
        };
        ordering.then_with(|| left.pid.cmp(&right.pid))
    });
}

fn compare_optional_u64(left: Option<u64>, right: Option<u64>) -> Ordering {
    match (left, right) {
        (Some(left), Some(right)) => left.cmp(&right),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

fn compare_optional_f32(left: Option<f32>, right: Option<f32>) -> Ordering {
    match (left, right) {
        (Some(left), Some(right)) => left.partial_cmp(&right).unwrap_or(Ordering::Equal),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

fn render_processes(processes: &[ProcessInfo], sort: SortKey) -> String {
    let mut processes = processes.to_vec();
    sort_processes(&mut processes, sort);
    let mut output = String::from("RustNT Process Inspector\n\n");
    output.push_str(&format!(
        "{:<8} {:<28} {:>8} {:>8} {:>12} {:<60}\n",
        "PID", "PROCESS", "CPU", "THREADS", "MEMORY", "PATH"
    ));
    output.push_str(&format!("{}\n", "-".repeat(124)));

    for process in processes {
        let cpu = process
            .cpu_percent
            .map(|value| format!("{value:.1}%"))
            .unwrap_or_else(|| "N/A".to_string());
        let memory = process
            .memory_bytes
            .map(format_bytes)
            .unwrap_or_else(|| "N/A".to_string());
        let path = truncate_path(process.path.as_deref().unwrap_or("N/A"), 60);
        output.push_str(&format!(
            "{:<8} {:<28} {:>8} {:>8} {:>12} {:<60}\n",
            process.pid,
            truncate_text(&process.name, 28),
            cpu,
            process.thread_count,
            memory,
            path
        ));
    }

    output.trim_end().to_string()
}

fn run_watch(options: &Options, interval: u64) -> Result<(), Box<dyn std::error::Error>> {
    let mut previous = rustnt_core::sample_processes()?;
    loop {
        let visible = filter_processes(previous.processes().to_vec(), options);
        print!("\x1B[2J\x1B[H{}", render_processes(&visible, options.sort));
        io::stdout().flush()?;
        std::thread::sleep(Duration::from_secs(interval));

        let mut current = rustnt_core::sample_processes()?;
        current.apply_cpu_from(&previous);
        previous = current;
    }
}

fn truncate_text(value: &str, width: usize) -> String {
    if value.chars().count() <= width {
        return value.to_string();
    }
    value.chars().take(width).collect()
}

fn truncate_path(value: &str, width: usize) -> String {
    if value.chars().count() <= width {
        return value.to_string();
    }
    let suffix: String = value
        .chars()
        .rev()
        .take(width.saturating_sub(3))
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!("...{suffix}")
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", bytes, UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::{
        format_bytes, matches_filter, parse_options, render_processes, sort_processes, SortKey,
    };

    #[test]
    fn formats_bytes_with_binary_units() {
        assert_eq!(format_bytes(1_258_291), "1.2 MB");
    }

    #[test]
    fn parses_watch_filter_and_sort_options() {
        let args = strings(&["--watch", "2", "--filter", "edge", "--sort", "cpu"]);
        let options = parse_options(&args).expect("options should parse");
        assert_eq!(options.watch_seconds, Some(2));
        assert_eq!(options.filter.as_deref(), Some("edge"));
        assert_eq!(options.sort, SortKey::Cpu);
    }

    #[test]
    fn rejects_missing_or_zero_watch_interval() {
        assert!(parse_options(&strings(&["--watch"])).is_err());
        assert!(parse_options(&strings(&["--watch", "0"])).is_err());
    }

    #[test]
    fn rejects_duplicate_and_unknown_options() {
        assert!(parse_options(&strings(&["--sort", "pid", "--sort", "name"])).is_err());
        assert!(parse_options(&strings(&["--filter", "--sort"])).is_err());
        assert!(parse_options(&strings(&["--unknown"])).is_err());
    }

    #[test]
    fn filter_matches_name_and_path_case_insensitively() {
        let process = process(42, "Edge.exe", Some("C:\\Apps\\Microsoft\\Edge.exe"));
        assert!(matches_filter(&process, Some("MICROSOFT")));
        assert!(!matches_filter(&process, Some("not-found")));
    }

    #[test]
    fn sort_by_memory_places_unavailable_values_last() {
        let mut processes = vec![
            process_with_memory(1, Some(100)),
            process_with_memory(2, None),
            process_with_memory(3, Some(10)),
        ];
        sort_processes(&mut processes, SortKey::Memory);
        assert_eq!(
            processes
                .iter()
                .map(|process| process.pid)
                .collect::<Vec<_>>(),
            vec![3, 1, 2]
        );
    }

    #[test]
    fn render_includes_task_02_columns() {
        let output = render_processes(&[process(7, "demo.exe", None)], SortKey::Pid);
        assert!(output.contains("CPU"));
        assert!(output.contains("MEMORY"));
        assert!(output.contains("PATH"));
    }

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    fn process(pid: u32, name: &str, path: Option<&str>) -> rustnt_core::ProcessInfo {
        rustnt_core::ProcessInfo {
            pid,
            name: name.to_string(),
            thread_count: 1,
            memory_bytes: None,
            path: path.map(str::to_string),
            cpu_percent: None,
        }
    }

    fn process_with_memory(pid: u32, memory_bytes: Option<u64>) -> rustnt_core::ProcessInfo {
        rustnt_core::ProcessInfo {
            memory_bytes,
            ..process(pid, "demo.exe", None)
        }
    }
}
