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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ServiceCommand {
    Install,
    Uninstall,
    Start,
    Stop,
    Status,
    Identity,
}

struct Options {
    watch_seconds: Option<u64>,
    filter: Option<String>,
    sort: SortKey,
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("service") {
        return run_service_cli(&args[1..]);
    }

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

fn print_service_usage() {
    eprintln!("usage: rustnt service <install|uninstall|start|stop|status|identity>");
}

fn run_service_cli(args: &[String]) -> ExitCode {
    let command = match parse_service_command(args) {
        Ok(command) => command,
        Err(error) => {
            eprintln!("usage error: {error}");
            print_service_usage();
            return ExitCode::from(2);
        }
    };

    match run_service_command(command) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::from(1)
        }
    }
}

fn parse_service_command(args: &[String]) -> Result<ServiceCommand, String> {
    if args.len() != 1 {
        return Err("service requires exactly one command".to_string());
    }

    match args[0].as_str() {
        "install" => Ok(ServiceCommand::Install),
        "uninstall" => Ok(ServiceCommand::Uninstall),
        "start" => Ok(ServiceCommand::Start),
        "stop" => Ok(ServiceCommand::Stop),
        "status" => Ok(ServiceCommand::Status),
        "identity" => Ok(ServiceCommand::Identity),
        command => Err(format!("unknown service command: {command}")),
    }
}

fn service_binary_path() -> Result<std::path::PathBuf, String> {
    let current_exe =
        env::current_exe().map_err(|error| format!("resolve current executable: {error}"))?;
    let parent = current_exe
        .parent()
        .ok_or_else(|| "resolve current executable directory".to_string())?;
    Ok(parent.join("rustnt-service.exe"))
}

fn run_service_command(command: ServiceCommand) -> Result<(), String> {
    match command {
        ServiceCommand::Install => {
            let binary_path = service_binary_path()?;
            rustnt_core::service::install_service(&binary_path).map_err(|error| error.to_string())
        }
        ServiceCommand::Uninstall => {
            rustnt_core::service::uninstall_service().map_err(|error| error.to_string())
        }
        ServiceCommand::Start => {
            rustnt_core::service::start_service().map_err(|error| error.to_string())
        }
        ServiceCommand::Stop => {
            rustnt_core::service::stop_service().map_err(|error| error.to_string())
        }
        ServiceCommand::Status => {
            let status =
                rustnt_core::service::query_service_status().map_err(|error| error.to_string())?;
            println!("{}", render_service_status(status));
            Ok(())
        }
        ServiceCommand::Identity => run_identity_command(),
    }
}

fn render_service_status(status: rustnt_core::service::ServiceStatus) -> String {
    let state = match status.state {
        rustnt_core::service::ServiceState::NotInstalled => "NOT_INSTALLED".to_string(),
        rustnt_core::service::ServiceState::Stopped => "STOPPED".to_string(),
        rustnt_core::service::ServiceState::StartPending => "START_PENDING".to_string(),
        rustnt_core::service::ServiceState::Running => "RUNNING".to_string(),
        rustnt_core::service::ServiceState::StopPending => "STOP_PENDING".to_string(),
        rustnt_core::service::ServiceState::Other(value) => format!("OTHER({value})"),
    };
    let process = status
        .process_id
        .map(|process_id| process_id.to_string())
        .unwrap_or_else(|| "N/A".to_string());

    format!(
        "RustNT Service\n\nSERVICE       RustNTControl\nSTATE         {state}\nPROCESS       {process}"
    )
}

fn render_identity(identity: rustnt_core::service::ServiceIdentity) -> String {
    format!(
        "RustNT Service Identity\n\nSERVICE       {}\nACCOUNT       {}\nACCOUNT SID   {}\nINTEGRITY     {}\nELEVATED      {}\nPROTOCOL      {}\nCAPABILITIES  {}",
        identity.service,
        identity.account,
        identity.account_sid,
        identity.integrity,
        identity.elevated,
        identity.protocol_version,
        identity.capabilities.join(",")
    )
}

fn run_identity_command() -> Result<(), String> {
    let status = rustnt_core::service::query_service_status().map_err(|error| error.to_string())?;
    if matches!(
        status.state,
        rustnt_core::service::ServiceState::NotInstalled
            | rustnt_core::service::ServiceState::Stopped
    ) {
        return Err("service is not running; run rustnt service start".to_string());
    }

    let response = rustnt_core::service::ServiceClient
        .request(rustnt_core::service::Command::Identity)
        .map_err(|error| error.to_string())?;
    if response.status != 0 {
        return Err(format!(
            "service identity request failed with status {}",
            response.status
        ));
    }

    let identity = decode_identity(&response.payload)?;
    println!("{}", render_identity(identity));
    Ok(())
}

fn decode_identity(payload: &[u8]) -> Result<rustnt_core::service::ServiceIdentity, String> {
    let payload = String::from_utf8(payload.to_vec())
        .map_err(|error| format!("decode service identity payload: {error}"))?;
    let mut fields = std::collections::HashMap::new();
    for line in payload.lines() {
        let (key, value) = line
            .split_once('=')
            .ok_or_else(|| "decode service identity payload: malformed field".to_string())?;
        if fields.insert(key, value).is_some() {
            return Err(format!(
                "decode service identity payload: duplicate field {key}"
            ));
        }
    }

    let field = |key: &str| {
        fields
            .get(key)
            .copied()
            .ok_or_else(|| format!("decode service identity payload: missing field {key}"))
    };
    let capabilities = field("CAPABILITIES")?
        .split(',')
        .filter(|capability| !capability.is_empty())
        .map(str::to_string)
        .collect();

    Ok(rustnt_core::service::ServiceIdentity {
        service: field("SERVICE")?.to_string(),
        account: field("ACCOUNT")?.to_string(),
        account_sid: field("ACCOUNT_SID")?.to_string(),
        integrity: field("INTEGRITY")?.to_string(),
        elevated: field("ELEVATED")?.parse().map_err(|error| {
            format!("decode service identity payload: invalid ELEVATED: {error}")
        })?,
        protocol_version: field("PROTOCOL")?.parse().map_err(|error| {
            format!("decode service identity payload: invalid PROTOCOL: {error}")
        })?,
        capabilities,
    })
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
        format_bytes, matches_filter, parse_options, parse_service_command, render_identity,
        render_processes, sort_processes, ServiceCommand, SortKey,
    };

    #[test]
    fn parses_all_service_commands() {
        assert_eq!(
            parse_service_command(&strings(&["install"])).expect("install should parse"),
            ServiceCommand::Install
        );
        assert_eq!(
            parse_service_command(&strings(&["identity"])).expect("identity should parse"),
            ServiceCommand::Identity
        );
    }

    #[test]
    fn rejects_unknown_or_extra_service_arguments() {
        assert!(parse_service_command(&strings(&["unknown"])).is_err());
        assert!(parse_service_command(&strings(&["start", "extra"])).is_err());
    }

    #[test]
    fn renders_identity_fields() {
        let output = render_identity(sample_identity());
        assert!(output.contains("ACCOUNT       LocalSystem"));
        assert!(output.contains("INTEGRITY     System"));
        assert!(output.contains("ELEVATED      true"));
        assert!(output.contains("CAPABILITIES  ping,identity,capabilities"));
    }

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

    fn sample_identity() -> rustnt_core::service::ServiceIdentity {
        rustnt_core::service::ServiceIdentity {
            service: "RustNTControl".to_string(),
            account: "LocalSystem".to_string(),
            account_sid: "S-1-5-18".to_string(),
            integrity: "System".to_string(),
            elevated: true,
            protocol_version: 1,
            capabilities: vec![
                "ping".to_string(),
                "identity".to_string(),
                "capabilities".to_string(),
            ],
        }
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
