#![cfg(windows)]

use std::cmp::Ordering;
use std::env;
use std::io::{self, Write};
use std::process::ExitCode;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rustnt_core::filesystem::{
    AllowOrDeny, FileKind, FileMetadata, FilePermissions, SearchLimits, SearchReport,
};
use rustnt_core::monitor::{MonitorSampler, MonitorSnapshot};
use rustnt_core::ProcessInfo;
use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};
use windows_sys::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};

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

#[derive(Debug, Clone, PartialEq, Eq)]
enum ProcessCommand {
    List(Options),
    Inspect { pid: u32 },
    Terminate { pid: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum FileSystemCommand {
    Stat { path: String },
    List { path: String },
    Space { path: String },
    Permissions { path: String },
    Search { path: String, name: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Options {
    watch_seconds: Option<u64>,
    filter: Option<String>,
    sort: SortKey,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MonitorOptions {
    watch_seconds: Option<u64>,
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("service") {
        return run_service_cli(&args[1..]);
    }

    if args.first().map(String::as_str) == Some("monitor") {
        let options = match parse_monitor_options(&args[1..]) {
            Ok(options) => options,
            Err(error) => {
                eprintln!("usage error: {error}");
                print_usage();
                return ExitCode::from(2);
            }
        };
        return match run_monitor_command(options) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("error: {error}");
                ExitCode::from(1)
            }
        };
    }

    if args.first().map(String::as_str) == Some("fs") {
        let command = match parse_filesystem_command(&args[1..]) {
            Ok(command) => command,
            Err(error) => {
                eprintln!("usage error: {error}");
                print_usage();
                return ExitCode::from(2);
            }
        };
        return match run_filesystem_command(command) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("error: {error}");
                ExitCode::from(1)
            }
        };
    }

    if args.first().map(String::as_str) != Some("process") {
        print_usage();
        return ExitCode::from(2);
    }

    let command = match parse_process_command(&args[1..]) {
        Ok(command) => command,
        Err(error) => {
            eprintln!("usage error: {error}");
            print_usage();
            return ExitCode::from(2);
        }
    };

    match run_process_command(command) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::from(1)
        }
    }
}

fn print_usage() {
    eprintln!(
        "usage: rustnt process list [--watch <seconds>] [--filter <text>] \
         [--sort <pid|name|cpu|memory>]\n\
         usage: rustnt process inspect --pid <PID>\n\
         usage: rustnt process terminate --pid <PID>\n\
         usage: rustnt monitor [--watch [seconds]]\n\
         usage: rustnt fs stat --path <path>\n\
         usage: rustnt fs list --path <directory>\n\
         usage: rustnt fs space --path <path>\n\
         usage: rustnt fs permissions --path <path>\n\
         usage: rustnt fs search --path <directory> --name <text>"
    );
}

fn parse_monitor_options(args: &[String]) -> Result<MonitorOptions, String> {
    let mut watch_seconds = None;
    let mut index = 0;
    while index < args.len() {
        if args[index] != "--watch" {
            return Err(format!("unknown monitor option: {}", args[index]));
        }
        if watch_seconds.is_some() {
            return Err("duplicate --watch".to_string());
        }
        let seconds = match args.get(index + 1) {
            None => 1,
            Some(value) if value.starts_with("--") => 1,
            Some(value) => {
                let seconds = value
                    .parse::<u64>()
                    .map_err(|_| "--watch requires a positive integer".to_string())?;
                if seconds == 0 {
                    return Err("--watch must be greater than zero".to_string());
                }
                index += 1;
                seconds
            }
        };
        watch_seconds = Some(seconds);
        index += 1;
    }
    Ok(MonitorOptions { watch_seconds })
}

fn parse_process_command(args: &[String]) -> Result<ProcessCommand, String> {
    let command = args
        .first()
        .map(String::as_str)
        .ok_or_else(|| "process requires a command".to_string())?;
    match command {
        "list" => parse_options(&args[1..]).map(ProcessCommand::List),
        "inspect" => parse_pid_option(&args[1..]).map(|pid| ProcessCommand::Inspect { pid }),
        "terminate" => parse_pid_option(&args[1..]).map(|pid| ProcessCommand::Terminate { pid }),
        command => Err(format!("unknown process command: {command}")),
    }
}

fn parse_pid_option(args: &[String]) -> Result<u32, String> {
    if args.len() != 2 || args[0] != "--pid" || args[1].starts_with('-') {
        return Err("process command requires exactly --pid <positive decimal PID>".to_string());
    }
    let pid = args[1]
        .parse::<u32>()
        .map_err(|_| "PID must be a positive decimal u32".to_string())?;
    if pid == 0 {
        return Err("PID must be greater than zero".to_string());
    }
    Ok(pid)
}

fn run_process_command(command: ProcessCommand) -> Result<(), String> {
    match command {
        ProcessCommand::List(options) => {
            if let Some(interval) = options.watch_seconds {
                run_watch(&options, interval).map_err(|error| error.to_string())
            } else {
                let processes = rustnt_core::list_processes().map_err(|error| error.to_string())?;
                let processes = filter_processes(processes, &options);
                println!("{}", render_processes(&processes, options.sort));
                Ok(())
            }
        }
        ProcessCommand::Inspect { pid } => run_process_inspect(pid),
        ProcessCommand::Terminate { pid } => run_process_terminate(pid),
    }
}

fn ensure_service_running() -> Result<(), String> {
    let status = rustnt_core::service::query_service_status().map_err(|error| error.to_string())?;
    if !service_state_is_ready(status.state) {
        Err("service is not running; run rustnt service start".to_string())
    } else {
        Ok(())
    }
}

fn service_state_is_ready(state: rustnt_core::service::ServiceState) -> bool {
    matches!(state, rustnt_core::service::ServiceState::Running)
}

fn request_process(
    command: rustnt_core::service::Command,
    payload: &[u8],
) -> Result<rustnt_core::service::Response, String> {
    rustnt_core::service::ServiceClient
        .request(command, payload)
        .map_err(|error| error.to_string())
}

fn run_process_inspect(pid: u32) -> Result<(), String> {
    ensure_service_running()?;
    let payload = rustnt_core::process_control::encode_inspect_request(pid)
        .map_err(|error| error.to_string())?;
    let response = request_process(rustnt_core::service::Command::ProcessInspect, &payload)?;
    if response.status != rustnt_core::service::STATUS_SUCCESS {
        return Err(render_process_error(&response.payload));
    }
    let inspection = rustnt_core::process_control::decode_inspection_payload(&response.payload)
        .map_err(|error| error.to_string())?;
    println!("{}", render_process_inspection(&inspection));
    Ok(())
}

fn run_process_terminate(pid: u32) -> Result<(), String> {
    ensure_service_running()?;
    let inspect_payload = rustnt_core::process_control::encode_inspect_request(pid)
        .map_err(|error| error.to_string())?;
    let inspection_response = request_process(
        rustnt_core::service::Command::ProcessInspect,
        &inspect_payload,
    )?;
    if inspection_response.status != rustnt_core::service::STATUS_SUCCESS {
        return Err(render_process_error(&inspection_response.payload));
    }
    let inspection =
        rustnt_core::process_control::decode_inspection_payload(&inspection_response.payload)
            .map_err(|error| error.to_string())?;
    let terminate_payload =
        rustnt_core::process_control::encode_terminate_request(pid, inspection.creation_time_100ns)
            .map_err(|error| error.to_string())?;
    let response = request_process(
        rustnt_core::service::Command::ProcessTerminate,
        &terminate_payload,
    )?;
    if response.status != rustnt_core::service::STATUS_SUCCESS {
        return Err(render_process_error(&response.payload));
    }
    let status = rustnt_core::process_control::decode_status_payload(&response.payload)
        .map_err(|error| error.to_string())?;
    println!("{}", render_process_status(status));
    Ok(())
}

fn render_process_error(payload: &[u8]) -> String {
    match rustnt_core::process_control::decode_status_payload(payload) {
        Ok(status) => format!("process operation failed: {}", process_status_name(status)),
        Err(_) => {
            "process operation failed: service returned an invalid error response".to_string()
        }
    }
}

fn render_process_status(status: rustnt_core::process_control::ProcessStatus) -> String {
    let detail = if matches!(
        status,
        rustnt_core::process_control::ProcessStatus::TerminatePending
    ) {
        "\ntermination accepted but is still pending; inspect the process again"
    } else {
        ""
    };
    let output = format!(
        "RustNT Process Termination\n\nSTATUS        {}",
        process_status_name(status)
    );
    format!("{output}{detail}")
}

fn process_status_name(status: rustnt_core::process_control::ProcessStatus) -> &'static str {
    match status {
        rustnt_core::process_control::ProcessStatus::Terminated => "TERMINATED",
        rustnt_core::process_control::ProcessStatus::TerminatePending => "TERMINATE_PENDING",
        rustnt_core::process_control::ProcessStatus::TargetNotFound => "TARGET_NOT_FOUND",
        rustnt_core::process_control::ProcessStatus::TargetAccessDenied => "TARGET_ACCESS_DENIED",
        rustnt_core::process_control::ProcessStatus::PidReused => "PID_REUSED",
        rustnt_core::process_control::ProcessStatus::TargetNotOwned => "TARGET_NOT_OWNED",
        rustnt_core::process_control::ProcessStatus::TargetProtected => "TARGET_PROTECTED",
        rustnt_core::process_control::ProcessStatus::CallerNotElevated => "CALLER_NOT_ELEVATED",
        rustnt_core::process_control::ProcessStatus::CallerNotAdmin => "CALLER_NOT_ADMIN",
        rustnt_core::process_control::ProcessStatus::AccessDenied => "ACCESS_DENIED",
    }
}

fn render_process_inspection(
    inspection: &rustnt_core::process_control::ProcessInspection,
) -> String {
    format!(
        "RustNT Process Inspection\n\nPID           {}\nCREATION      {}\nIMAGE         {}\nPATH          {}\nTHREADS       {}\nMEMORY        {}\nOWNER SID     {}",
        inspection.pid,
        inspection.creation_time_100ns,
        inspection.image_name,
        inspection.image_path.as_deref().unwrap_or("N/A"),
        inspection.thread_count,
        inspection
            .memory_bytes
            .map(|value| value.to_string())
            .unwrap_or_else(|| "N/A".to_string()),
        inspection.owner_sid
    )
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
        .request(rustnt_core::service::Command::Identity, &[])
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

fn run_monitor_command(options: MonitorOptions) -> Result<(), String> {
    match options.watch_seconds {
        Some(interval) => run_monitor_watch(interval).map_err(|error| error.to_string()),
        None => {
            let mut sampler = MonitorSampler::new();
            let snapshot = sampler.sample().map_err(|error| error.to_string())?;
            println!("{}", render_monitor(&snapshot));
            Ok(())
        }
    }
}

fn run_monitor_watch(interval: u64) -> Result<(), Box<dyn std::error::Error>> {
    let mut sampler = MonitorSampler::new();
    loop {
        let snapshot = sampler.sample()?;
        print!("\x1B[2J\x1B[H{}", render_monitor(&snapshot));
        io::stdout().flush()?;
        std::thread::sleep(Duration::from_secs(interval));
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

fn render_monitor(snapshot: &MonitorSnapshot) -> String {
    let cpu = snapshot
        .cpu_percent
        .map(|value| format!("{value:.1}%"))
        .unwrap_or_else(|| "N/A".to_string());
    let memory_percent = snapshot
        .memory
        .used_percent()
        .map(|value| format!("{value:.1}%"))
        .unwrap_or_else(|| "N/A".to_string());
    let mut output = format!(
        "RustNT System Monitor\n\nCPU              {cpu}\nMEMORY           {} / {} ({memory_percent})\n\nDISKS\nROOT             FREE        TOTAL       USED",
        format_bytes(snapshot.memory.used_bytes()),
        format_bytes(snapshot.memory.total_bytes)
    );

    for disk in &snapshot.disks {
        let free = disk
            .free_bytes
            .map(format_bytes)
            .unwrap_or_else(|| "N/A".to_string());
        let total = disk
            .total_bytes
            .map(format_bytes)
            .unwrap_or_else(|| "N/A".to_string());
        let used = disk
            .used_percent()
            .map(|value| format!("{value:.1}%"))
            .unwrap_or_else(|| "N/A".to_string());
        output.push_str(&format!(
            "\n{:<17}{:<12}{:<12}{used}",
            disk.root, free, total
        ));
    }

    output.push_str("\n\nPROCESS CHANGES");
    render_process_changes(&mut output, "NEW", &snapshot.process_changes.added);
    render_process_changes(&mut output, "EXITED", &snapshot.process_changes.exited);
    output
}

fn render_process_changes(
    output: &mut String,
    label: &str,
    changes: &[rustnt_core::monitor::ProcessChange],
) {
    if changes.is_empty() {
        output.push_str(&format!("\n{label:<17}none"));
        return;
    }
    for (index, change) in changes.iter().enumerate() {
        let label = if index == 0 { label } else { "" };
        output.push_str(&format!("\n{label:<17}{} {}", change.pid, change.name));
    }
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests {
    use super::{
        format_bytes, matches_filter, parse_filesystem_command, parse_monitor_options,
        parse_options, parse_process_command, parse_service_command, render_disk_space,
        render_file_metadata, render_identity, render_monitor, render_permissions,
        render_process_inspection, render_process_status, render_processes, render_search,
        service_state_is_ready, sort_processes, FileSystemCommand, ProcessCommand, ServiceCommand,
        SortKey,
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
    fn parses_monitor_with_default_one_second_watch() {
        assert_eq!(
            parse_monitor_options(&strings(&["--watch"]))
                .expect("monitor watch should parse")
                .watch_seconds,
            Some(1)
        );
    }

    #[test]
    fn parses_monitor_with_explicit_watch_interval() {
        assert_eq!(
            parse_monitor_options(&strings(&["--watch", "3"]))
                .expect("monitor interval should parse")
                .watch_seconds,
            Some(3)
        );
    }

    #[test]
    fn rejects_invalid_monitor_options() {
        for args in [
            vec!["--watch", "0"],
            vec!["--watch", "-1"],
            vec!["--watch", "not-a-number"],
            vec!["--watch", "2", "--watch", "3"],
            vec!["--unknown"],
        ] {
            assert!(parse_monitor_options(&strings(&args)).is_err());
        }
    }

    #[test]
    fn render_monitor_sections_and_process_changes() {
        let snapshot = rustnt_core::monitor::MonitorSnapshot {
            cpu_percent: None,
            memory: rustnt_core::monitor::MemoryInfo {
                total_bytes: 4096,
                available_bytes: 1024,
            },
            disks: vec![rustnt_core::monitor::DiskInfo {
                root: "C:\\".to_string(),
                total_bytes: None,
                free_bytes: None,
            }],
            process_changes: rustnt_core::monitor::ProcessChanges {
                added: vec![rustnt_core::monitor::ProcessChange {
                    pid: 42,
                    name: "new.exe".to_string(),
                }],
                exited: Vec::new(),
            },
        };

        let output = render_monitor(&snapshot);
        assert!(output.contains("CPU"));
        assert!(output.contains("MEMORY"));
        assert!(output.contains("DISKS"));
        assert!(output.contains("PROCESS CHANGES"));
        assert!(output.contains("N/A"));
        assert!(output.contains("42 new.exe"));
        assert!(output.contains("EXITED           none"));
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
    fn parses_inspect_and_terminate_with_positive_decimal_pid() {
        assert_eq!(
            parse_process_command(&strings(&["inspect", "--pid", "42"]))
                .expect("inspect should parse"),
            ProcessCommand::Inspect { pid: 42 }
        );
        assert_eq!(
            parse_process_command(&strings(&["terminate", "--pid", "42"]))
                .expect("terminate should parse"),
            ProcessCommand::Terminate { pid: 42 }
        );
    }

    #[test]
    fn rejects_invalid_process_command_pid_and_arguments() {
        for args in [
            vec!["inspect"],
            vec!["inspect", "--pid"],
            vec!["inspect", "--pid", "0"],
            vec!["inspect", "--pid", "-1"],
            vec!["inspect", "--pid", "not-a-pid"],
            vec!["inspect", "--pid", "42", "--extra"],
            vec!["terminate", "--name", "demo.exe"],
            vec!["terminate", "--pid", "42", "--force"],
            vec!["unknown", "--pid", "42"],
        ] {
            assert!(
                parse_process_command(&strings(&args)).is_err(),
                "expected rejection for {args:?}"
            );
        }
    }

    #[test]
    fn renders_bounded_inspection_fields_without_unlisted_metadata() {
        let inspection = rustnt_core::process_control::ProcessInspection {
            pid: 42,
            creation_time_100ns: 1234,
            image_name: "demo.exe".to_string(),
            image_path: Some(r"C:\Apps\demo.exe".to_string()),
            thread_count: 3,
            memory_bytes: Some(4096),
            owner_sid: "S-1-5-21-user".to_string(),
        };
        let output = render_process_inspection(&inspection);
        assert!(output.contains("PID           42"));
        assert!(output.contains("CREATION      1234"));
        assert!(output.contains("OWNER SID     S-1-5-21-user"));
        assert!(!output.contains("COMMAND_LINE"));
        assert!(!output.contains("TOKEN"));
    }

    #[test]
    fn renders_pending_termination_with_reinspect_instruction() {
        let output =
            render_process_status(rustnt_core::process_control::ProcessStatus::TerminatePending);
        assert!(output.contains("TERMINATE_PENDING"));
        assert!(output.contains("inspect the process again"));
    }

    #[test]
    fn process_commands_require_the_service_to_be_running() {
        assert!(service_state_is_ready(
            rustnt_core::service::ServiceState::Running
        ));
        for state in [
            rustnt_core::service::ServiceState::NotInstalled,
            rustnt_core::service::ServiceState::Stopped,
            rustnt_core::service::ServiceState::StartPending,
            rustnt_core::service::ServiceState::StopPending,
            rustnt_core::service::ServiceState::Other(999),
        ] {
            assert!(!service_state_is_ready(state));
        }
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

    #[test]
    fn parses_all_filesystem_commands() {
        assert_eq!(
            parse_filesystem_command(&strings(&["stat", "--path", r"C:\Temp\a.txt"]))
                .expect("stat should parse"),
            FileSystemCommand::Stat {
                path: r"C:\Temp\a.txt".to_string()
            }
        );
        assert_eq!(
            parse_filesystem_command(&strings(&[
                "search", "--path", r"C:\Temp", "--name", "notes"
            ]))
            .expect("search should parse"),
            FileSystemCommand::Search {
                path: r"C:\Temp".to_string(),
                name: "notes".to_string()
            }
        );
        assert_eq!(
            parse_filesystem_command(&strings(&["list", "--path", r"C:\Temp\folder"]))
                .expect("list should parse"),
            FileSystemCommand::List {
                path: r"C:\Temp\folder".to_string()
            }
        );
        assert_eq!(
            parse_filesystem_command(&strings(&["space", "--path", r"C:\Temp\folder"]))
                .expect("space should parse"),
            FileSystemCommand::Space {
                path: r"C:\Temp\folder".to_string()
            }
        );
        assert_eq!(
            parse_filesystem_command(&strings(&[
                "permissions",
                "--path",
                r"C:\Temp\folder with spaces\file.txt"
            ]))
            .expect("permissions should parse"),
            FileSystemCommand::Permissions {
                path: r"C:\Temp\folder with spaces\file.txt".to_string()
            }
        );
    }

    #[test]
    fn rejects_invalid_filesystem_arguments() {
        for args in [
            vec!["stat"],
            vec!["list", "--path"],
            vec!["space", "--path", "x", "--path", "y"],
            vec!["permissions", "--unknown", "x"],
            vec!["search", "--path", "x"],
            vec!["search", "--path", "x", "--name", ""],
            vec!["unknown", "--path", "x"],
        ] {
            assert!(
                parse_filesystem_command(&strings(&args)).is_err(),
                "expected rejection for {args:?}"
            );
        }
    }

    #[test]
    fn renders_filesystem_missing_values_and_search_summary() {
        let output = render_file_metadata(&sample_directory_metadata_with_missing_times());
        assert!(output.contains("TYPE              DIRECTORY"));
        assert!(output.contains("SIZE              N/A"));
        assert!(output.contains("CREATED           N/A"));

        let output = render_search(&sample_search_report());
        assert!(output.contains("MATCHES"));
        assert!(output.contains("SKIPPED ACCESS"));
        assert!(output.contains("TRUNCATED         no"));
    }

    #[test]
    fn renders_filesystem_space_in_fixed_field_order() {
        let output = render_disk_space(&rustnt_core::filesystem::DiskSpace {
            root: r"C:\".to_string(),
            free_bytes: 40,
            total_bytes: 100,
            available_bytes: 25,
        });
        let free = output.find("FREE").expect("FREE should render");
        let available = output.find("AVAILABLE").expect("AVAILABLE should render");
        let total = output.find("TOTAL").expect("TOTAL should render");
        let used = output.find("USED").expect("USED should render");
        assert!(free < available);
        assert!(available < total);
        assert!(total < used);
        assert!(output.contains("USED              60.0%"));
    }

    #[test]
    fn renders_filesystem_permissions_and_ace_rows() {
        let output = render_permissions(&rustnt_core::filesystem::FilePermissions {
            owner_sid: Some("S-1-5-18".to_string()),
            dacl_present: true,
            dacl_protected: false,
            entries: vec![rustnt_core::filesystem::AceEntry {
                kind: rustnt_core::filesystem::AllowOrDeny::Allow,
                sid: "S-1-1-0".to_string(),
                mask: 0x120089,
                inherited: true,
            }],
        });
        assert!(output.contains("OWNER SID        S-1-5-18"));
        assert!(output.contains("DACL PRESENT     yes"));
        assert!(output.contains("DACL PROTECTED   no"));
        assert!(output.contains("ALLOW"));
        assert!(output.contains("0x00120089"));
        assert!(output.contains("S-1-1-0"));
        assert!(output.contains("yes"));
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

    fn sample_directory_metadata_with_missing_times() -> rustnt_core::filesystem::FileMetadata {
        rustnt_core::filesystem::FileMetadata {
            path: r"C:\Temp".to_string(),
            kind: rustnt_core::filesystem::FileKind::Directory,
            size_bytes: None,
            created: None,
            modified: None,
            accessed: None,
            attributes: 0x10,
            is_reparse_point: false,
        }
    }

    fn sample_search_report() -> rustnt_core::filesystem::SearchReport {
        rustnt_core::filesystem::SearchReport {
            matches: vec![rustnt_core::filesystem::FileMetadata {
                path: r"C:\Temp\notes.txt".to_string(),
                kind: rustnt_core::filesystem::FileKind::File,
                size_bytes: Some(4),
                created: None,
                modified: None,
                accessed: None,
                attributes: 0,
                is_reparse_point: false,
            }],
            skipped_access: 2,
            skipped_reparse: 1,
            truncated: false,
        }
    }
}

fn parse_filesystem_command(args: &[String]) -> Result<FileSystemCommand, String> {
    let command = args
        .first()
        .map(String::as_str)
        .ok_or_else(|| "fs requires a command".to_string())?;
    if !matches!(
        command,
        "stat" | "list" | "space" | "permissions" | "search"
    ) {
        return Err(format!("unknown fs command: {command}"));
    }

    let mut path = None;
    let mut name = None;
    let mut index = 1;
    while index < args.len() {
        let flag = args[index].as_str();
        let value = args
            .get(index + 1)
            .ok_or_else(|| format!("{flag} requires a value"))?;
        if value.starts_with("--") || value.is_empty() {
            return Err(format!("{flag} requires a non-empty value"));
        }
        match flag {
            "--path" => {
                if path.is_some() {
                    return Err("duplicate --path".to_string());
                }
                path = Some(value.clone());
            }
            "--name" if command == "search" => {
                if name.is_some() {
                    return Err("duplicate --name".to_string());
                }
                name = Some(value.clone());
            }
            "--name" => return Err("--name is only valid for search".to_string()),
            _ => return Err(format!("unknown fs option: {flag}")),
        }
        index += 2;
    }

    let path = path.ok_or_else(|| "fs command requires exactly --path <path>".to_string())?;
    match command {
        "stat" => Ok(FileSystemCommand::Stat { path }),
        "list" => Ok(FileSystemCommand::List { path }),
        "space" => Ok(FileSystemCommand::Space { path }),
        "permissions" => Ok(FileSystemCommand::Permissions { path }),
        "search" => Ok(FileSystemCommand::Search {
            path,
            name: name.ok_or_else(|| "search requires exactly --name <text>".to_string())?,
        }),
        _ => unreachable!("fs command was validated above"),
    }
}

fn run_filesystem_command(command: FileSystemCommand) -> Result<(), String> {
    match command {
        FileSystemCommand::Stat { path } => {
            let metadata =
                rustnt_core::filesystem::stat_path(&path).map_err(format_filesystem_error)?;
            println!("{}", render_file_metadata(&metadata));
        }
        FileSystemCommand::List { path } => {
            let entries =
                rustnt_core::filesystem::list_directory(&path).map_err(format_filesystem_error)?;
            println!("{}", render_directory_entries(&entries));
        }
        FileSystemCommand::Space { path } => {
            let space =
                rustnt_core::filesystem::disk_space(&path).map_err(format_filesystem_error)?;
            println!("{}", render_disk_space(&space));
        }
        FileSystemCommand::Permissions { path } => {
            let permissions = rustnt_core::filesystem::read_permissions(&path)
                .map_err(format_filesystem_error)?;
            println!("{}", render_permissions(&permissions));
        }
        FileSystemCommand::Search { path, name } => {
            let report = rustnt_core::filesystem::search_path(
                &path,
                &name,
                SearchLimits {
                    max_depth: 16,
                    max_results: 1000,
                },
            )
            .map_err(format_filesystem_error)?;
            println!("{}", render_search(&report));
        }
    }
    Ok(())
}

fn format_filesystem_error(error: rustnt_core::filesystem::FileSystemError) -> String {
    match error {
        rustnt_core::filesystem::FileSystemError::InvalidPath(path) => {
            format!("invalid path: {path}")
        }
        rustnt_core::filesystem::FileSystemError::NotFound(path) => {
            format!("path not found: {path}")
        }
        rustnt_core::filesystem::FileSystemError::AccessDenied(path) => {
            format!("access denied: {path}")
        }
        rustnt_core::filesystem::FileSystemError::Io(message) => format!("I/O error: {message}"),
        rustnt_core::filesystem::FileSystemError::Win32 { operation, code } => {
            format!("{operation}: Windows error {code}")
        }
    }
}

fn file_kind_name(kind: &FileKind) -> &'static str {
    match kind {
        FileKind::File => "FILE",
        FileKind::Directory => "DIRECTORY",
        FileKind::ReparsePoint => "REPARSE",
        FileKind::Other => "OTHER",
    }
}

fn final_path_component(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

fn format_optional_bytes(value: Option<u64>) -> String {
    value.map(format_bytes).unwrap_or_else(|| "N/A".to_string())
}

fn format_filetime(value: Option<SystemTime>) -> String {
    value
        .and_then(format_local_system_time)
        .unwrap_or_else(|| "N/A".to_string())
}

fn format_local_system_time(value: SystemTime) -> Option<String> {
    const WINDOWS_EPOCH_OFFSET_100NS: u128 = 116_444_736_000_000_000;
    let elapsed = value.duration_since(UNIX_EPOCH).ok()?;
    let ticks = elapsed
        .as_nanos()
        .checked_div(100)?
        .checked_add(WINDOWS_EPOCH_OFFSET_100NS)?;
    let ticks = u64::try_from(ticks).ok()?;
    let file_time = FILETIME {
        dwLowDateTime: ticks as u32,
        dwHighDateTime: (ticks >> 32) as u32,
    };
    let mut utc = SYSTEMTIME {
        ..unsafe { std::mem::zeroed() }
    };
    let ok = unsafe {
        // SAFETY: file_time points to a valid FILETIME and utc points to writable storage.
        FileTimeToSystemTime(&file_time, &mut utc)
    };
    if ok == 0 {
        return None;
    }
    let mut local = SYSTEMTIME {
        ..unsafe { std::mem::zeroed() }
    };
    let ok = unsafe {
        // SAFETY: utc points to a valid SYSTEMTIME and local points to writable storage.
        SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc, &mut local)
    };
    if ok == 0 {
        return None;
    }
    Some(format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        local.wYear, local.wMonth, local.wDay, local.wHour, local.wMinute, local.wSecond
    ))
}

fn render_file_metadata(metadata: &FileMetadata) -> String {
    format!(
        "PATH              {}\nTYPE              {}\nATTRIBUTES        0x{:08X}\nSIZE              {}\nCREATED           {}\nMODIFIED          {}\nACCESSED          {}\nREPARSE POINT     {}",
        metadata.path,
        file_kind_name(&metadata.kind),
        metadata.attributes,
        format_optional_bytes(metadata.size_bytes),
        format_filetime(metadata.created),
        format_filetime(metadata.modified),
        format_filetime(metadata.accessed),
        if metadata.is_reparse_point { "yes" } else { "no" }
    )
}

fn render_directory_entries(entries: &[rustnt_core::filesystem::DirectoryEntry]) -> String {
    let mut output = String::from("TYPE        SIZE        NAME");
    for entry in entries {
        let size = if entry.metadata.is_reparse_point || entry.metadata.kind == FileKind::Directory
        {
            "N/A".to_string()
        } else {
            format_optional_bytes(entry.metadata.size_bytes)
        };
        output.push_str(&format!(
            "\n{:<11}{:<12}{}",
            file_kind_name(&entry.metadata.kind),
            size,
            final_path_component(&entry.metadata.path)
        ));
    }
    output
}

fn render_disk_space(space: &rustnt_core::filesystem::DiskSpace) -> String {
    let used = if space.total_bytes == 0 || space.free_bytes > space.total_bytes {
        "N/A".to_string()
    } else {
        let used_bytes = space.total_bytes - space.free_bytes;
        format!(
            "{:.1}%",
            (used_bytes as f64 / space.total_bytes as f64) * 100.0
        )
    };
    format!(
        "ROOT              {}\nFREE              {}\nAVAILABLE         {}\nTOTAL             {}\nUSED              {}",
        space.root,
        format_bytes(space.free_bytes),
        format_bytes(space.available_bytes),
        format_bytes(space.total_bytes),
        used
    )
}

fn render_permissions(permissions: &FilePermissions) -> String {
    let mut output = format!(
        "{:<17}{}\n{:<17}{}\n{:<17}{}",
        "OWNER SID",
        permissions.owner_sid.as_deref().unwrap_or("N/A"),
        "DACL PRESENT",
        if permissions.dacl_present {
            "yes"
        } else {
            "no"
        },
        "DACL PROTECTED",
        if permissions.dacl_protected {
            "yes"
        } else {
            "no"
        },
    );
    output.push_str("\n\nTYPE      SID              MASK          INHERITED");
    for entry in &permissions.entries {
        let kind = match entry.kind {
            AllowOrDeny::Allow => "ALLOW",
            AllowOrDeny::Deny => "DENY",
        };
        output.push_str(&format!(
            "\n{:<10}{:<17} 0x{:08X}    {}",
            kind,
            entry.sid,
            entry.mask,
            if entry.inherited { "yes" } else { "no" }
        ));
    }
    output
}

fn render_search(report: &SearchReport) -> String {
    let mut output = report
        .matches
        .iter()
        .map(|metadata| metadata.path.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    if output.is_empty() {
        output.push('\n');
    } else {
        output.push_str("\n\n");
    }
    output.push_str(&format!(
        "MATCHES             {}\nSKIPPED ACCESS      {}\nSKIPPED REPARSE     {}\nTRUNCATED         {}",
        report.matches.len(),
        report.skipped_access,
        report.skipped_reparse,
        if report.truncated { "yes" } else { "no" }
    ));
    output
}
