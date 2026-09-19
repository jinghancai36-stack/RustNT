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

#[derive(Debug, Clone, PartialEq, Eq)]
enum ProcessCommand {
    List(Options),
    Inspect { pid: u32 },
    Terminate { pid: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
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
         usage: rustnt process terminate --pid <PID>"
    );
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
        format_bytes, matches_filter, parse_options, parse_process_command, parse_service_command,
        render_identity, render_process_inspection, render_process_status, render_processes,
        service_state_is_ready, sort_processes, ProcessCommand, ServiceCommand, SortKey,
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
