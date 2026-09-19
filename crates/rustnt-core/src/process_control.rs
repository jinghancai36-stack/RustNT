use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessStatus {
    Terminated,
    TerminatePending,
    TargetNotFound,
    TargetAccessDenied,
    PidReused,
    TargetNotOwned,
    TargetProtected,
    CallerNotElevated,
    CallerNotAdmin,
    AccessDenied,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessControlError {
    InvalidPayload(String),
    InvalidField(String),
    OversizedPayload,
    Status(ProcessStatus),
    Windows { operation: &'static str, code: u32 },
}

impl fmt::Display for ProcessControlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPayload(message) => write!(formatter, "invalid payload: {message}"),
            Self::InvalidField(field) => write!(formatter, "invalid field: {field}"),
            Self::OversizedPayload => write!(formatter, "payload exceeds configured limit"),
            Self::Status(status) => write!(formatter, "process operation status: {status:?}"),
            Self::Windows { operation, code } => {
                write!(formatter, "{operation}: Windows error {code}")
            }
        }
    }
}

impl std::error::Error for ProcessControlError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessInspectRequest {
    pub pid: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessTerminateRequest {
    pub pid: u32,
    pub expected_creation_time_100ns: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessInspection {
    pub pid: u32,
    pub creation_time_100ns: u64,
    pub image_name: String,
    pub image_path: Option<String>,
    pub thread_count: u32,
    pub memory_bytes: Option<u64>,
    pub owner_sid: String,
}

pub fn encode_inspect_request(pid: u32) -> Result<Vec<u8>, ProcessControlError> {
    validate_pid(pid)?;
    Ok(pid.to_le_bytes().to_vec())
}

pub fn decode_inspect_request(
    payload: &[u8],
) -> Result<ProcessInspectRequest, ProcessControlError> {
    if payload.len() != 4 {
        return Err(ProcessControlError::InvalidPayload(
            "inspect request must contain exactly 4 bytes".to_string(),
        ));
    }
    let pid = u32::from_le_bytes(
        payload
            .try_into()
            .expect("payload length was checked before conversion"),
    );
    validate_pid(pid)?;
    Ok(ProcessInspectRequest { pid })
}

pub fn encode_terminate_request(
    pid: u32,
    expected_creation_time_100ns: u64,
) -> Result<Vec<u8>, ProcessControlError> {
    validate_pid(pid)?;
    let mut payload = Vec::with_capacity(12);
    payload.extend_from_slice(&pid.to_le_bytes());
    payload.extend_from_slice(&expected_creation_time_100ns.to_le_bytes());
    Ok(payload)
}

pub fn decode_terminate_request(
    payload: &[u8],
) -> Result<ProcessTerminateRequest, ProcessControlError> {
    if payload.len() != 12 {
        return Err(ProcessControlError::InvalidPayload(
            "terminate request must contain exactly 12 bytes".to_string(),
        ));
    }
    let pid = u32::from_le_bytes(
        payload[..4]
            .try_into()
            .expect("payload length was checked before conversion"),
    );
    let expected_creation_time_100ns = u64::from_le_bytes(
        payload[4..]
            .try_into()
            .expect("payload length was checked before conversion"),
    );
    validate_pid(pid)?;
    Ok(ProcessTerminateRequest {
        pid,
        expected_creation_time_100ns,
    })
}

pub fn encode_inspection_payload(
    inspection: &ProcessInspection,
    max_payload: usize,
) -> Result<Vec<u8>, ProcessControlError> {
    validate_pid(inspection.pid)?;
    let image_name = sanitize_field(&inspection.image_name)?;
    let image_path = inspection
        .image_path
        .as_deref()
        .map(sanitize_field)
        .transpose()?
        .unwrap_or_else(|| "N/A".to_string());
    let owner_sid = sanitize_field(&inspection.owner_sid)?;
    let payload = format!(
        "PID={}\nCREATION_TIME_100NS={}\nIMAGE_NAME={}\nIMAGE_PATH={}\nTHREADS={}\nMEMORY_BYTES={}\nOWNER_SID={}\n",
        inspection.pid,
        inspection.creation_time_100ns,
        image_name,
        image_path,
        inspection.thread_count,
        inspection
            .memory_bytes
            .map(|value| value.to_string())
            .unwrap_or_else(|| "N/A".to_string()),
        owner_sid,
    )
    .into_bytes();
    if payload.len() > max_payload {
        return Err(ProcessControlError::OversizedPayload);
    }
    Ok(payload)
}

pub fn decode_inspection_payload(payload: &[u8]) -> Result<ProcessInspection, ProcessControlError> {
    let text = std::str::from_utf8(payload).map_err(|_| {
        ProcessControlError::InvalidPayload("inspection payload must be UTF-8".to_string())
    })?;
    let text = text.strip_suffix('\n').ok_or_else(|| {
        ProcessControlError::InvalidPayload(
            "inspection payload must end with a newline".to_string(),
        )
    })?;
    let expected_keys = [
        "PID",
        "CREATION_TIME_100NS",
        "IMAGE_NAME",
        "IMAGE_PATH",
        "THREADS",
        "MEMORY_BYTES",
        "OWNER_SID",
    ];
    let mut values = Vec::with_capacity(expected_keys.len());
    for (line, expected_key) in text.split('\n').zip(expected_keys) {
        let (key, value) = line.split_once('=').ok_or_else(|| {
            ProcessControlError::InvalidPayload("inspection field is malformed".to_string())
        })?;
        if key != expected_key {
            return Err(ProcessControlError::InvalidPayload(format!(
                "expected field {expected_key}"
            )));
        }
        values.push(value);
    }
    if text.split('\n').count() != expected_keys.len() {
        return Err(ProcessControlError::InvalidPayload(
            "inspection payload has an unexpected field count".to_string(),
        ));
    }

    let pid = parse_decimal(values[0], "PID")?;
    validate_pid(pid)?;
    let creation_time_100ns = parse_decimal(values[1], "CREATION_TIME_100NS")?;
    let image_name = sanitize_required(values[2], "IMAGE_NAME")?;
    let image_path = if values[3] == "N/A" {
        None
    } else {
        Some(sanitize_required(values[3], "IMAGE_PATH")?)
    };
    let thread_count = parse_decimal(values[4], "THREADS")?;
    let memory_bytes = if values[5] == "N/A" {
        None
    } else {
        Some(parse_decimal(values[5], "MEMORY_BYTES")?)
    };
    let owner_sid = sanitize_required(values[6], "OWNER_SID")?;

    Ok(ProcessInspection {
        pid,
        creation_time_100ns,
        image_name,
        image_path,
        thread_count,
        memory_bytes,
        owner_sid,
    })
}

pub fn encode_status_payload(status: ProcessStatus) -> Vec<u8> {
    format!("STATUS={}\n", status_name(status)).into_bytes()
}

pub fn decode_status_payload(payload: &[u8]) -> Result<ProcessStatus, ProcessControlError> {
    let text = std::str::from_utf8(payload).map_err(|_| {
        ProcessControlError::InvalidPayload("status payload must be UTF-8".to_string())
    })?;
    let value = text
        .strip_prefix("STATUS=")
        .and_then(|value| value.strip_suffix('\n'))
        .ok_or_else(|| {
            ProcessControlError::InvalidPayload("status payload is malformed".to_string())
        })?;
    status_from_name(value).ok_or_else(|| {
        ProcessControlError::InvalidPayload("status payload contains an unknown status".to_string())
    })
}

pub fn sanitize_field(value: &str) -> Result<String, ProcessControlError> {
    if value.is_empty()
        || value
            .chars()
            .any(|character| character == '=' || character.is_control())
    {
        return Err(ProcessControlError::InvalidField(
            "field contains a delimiter or control character".to_string(),
        ));
    }
    Ok(value.to_string())
}

fn validate_pid(pid: u32) -> Result<(), ProcessControlError> {
    if pid == 0 {
        Err(ProcessControlError::InvalidPayload(
            "PID must be greater than zero".to_string(),
        ))
    } else {
        Ok(())
    }
}

fn sanitize_required(value: &str, field: &str) -> Result<String, ProcessControlError> {
    sanitize_field(value).map_err(|_| ProcessControlError::InvalidField(field.to_string()))
}

fn parse_decimal<T>(value: &str, field: &str) -> Result<T, ProcessControlError>
where
    T: std::str::FromStr,
{
    value
        .parse()
        .map_err(|_| ProcessControlError::InvalidField(field.to_string()))
}

fn status_name(status: ProcessStatus) -> &'static str {
    match status {
        ProcessStatus::Terminated => "TERMINATED",
        ProcessStatus::TerminatePending => "TERMINATE_PENDING",
        ProcessStatus::TargetNotFound => "TARGET_NOT_FOUND",
        ProcessStatus::TargetAccessDenied => "TARGET_ACCESS_DENIED",
        ProcessStatus::PidReused => "PID_REUSED",
        ProcessStatus::TargetNotOwned => "TARGET_NOT_OWNED",
        ProcessStatus::TargetProtected => "TARGET_PROTECTED",
        ProcessStatus::CallerNotElevated => "CALLER_NOT_ELEVATED",
        ProcessStatus::CallerNotAdmin => "CALLER_NOT_ADMIN",
        ProcessStatus::AccessDenied => "ACCESS_DENIED",
    }
}

fn status_from_name(name: &str) -> Option<ProcessStatus> {
    Some(match name {
        "TERMINATED" => ProcessStatus::Terminated,
        "TERMINATE_PENDING" => ProcessStatus::TerminatePending,
        "TARGET_NOT_FOUND" => ProcessStatus::TargetNotFound,
        "TARGET_ACCESS_DENIED" => ProcessStatus::TargetAccessDenied,
        "PID_REUSED" => ProcessStatus::PidReused,
        "TARGET_NOT_OWNED" => ProcessStatus::TargetNotOwned,
        "TARGET_PROTECTED" => ProcessStatus::TargetProtected,
        "CALLER_NOT_ELEVATED" => ProcessStatus::CallerNotElevated,
        "CALLER_NOT_ADMIN" => ProcessStatus::CallerNotAdmin,
        "ACCESS_DENIED" => ProcessStatus::AccessDenied,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        decode_inspect_request, decode_inspection_payload, decode_status_payload,
        decode_terminate_request, encode_inspect_request, encode_inspection_payload,
        encode_status_payload, encode_terminate_request, ProcessInspection, ProcessStatus,
    };

    fn sample_inspection(image_name: &str) -> ProcessInspection {
        ProcessInspection {
            pid: 42,
            creation_time_100ns: 1234,
            image_name: image_name.to_string(),
            image_path: Some(r"C:\Apps\demo.exe".to_string()),
            thread_count: 3,
            memory_bytes: Some(4096),
            owner_sid: "S-1-5-21-100-200-300-1001".to_string(),
        }
    }

    #[test]
    fn inspect_request_uses_exact_little_endian_four_byte_payload() {
        assert_eq!(
            encode_inspect_request(0x1122_3344).expect("request should encode"),
            vec![0x44, 0x33, 0x22, 0x11]
        );
    }

    #[test]
    fn inspect_request_rejects_zero_truncated_and_trailing_payloads() {
        assert!(decode_inspect_request(&0u32.to_le_bytes()).is_err());
        assert!(decode_inspect_request(&[1, 0, 0]).is_err());
        assert!(decode_inspect_request(&[1, 0, 0, 0, 0]).is_err());
    }

    #[test]
    fn terminate_request_uses_exact_little_endian_twelve_byte_payload() {
        assert_eq!(
            encode_terminate_request(0x1122_3344, 0x0102_0304_0506_0708)
                .expect("request should encode"),
            vec![0x44, 0x33, 0x22, 0x11, 0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01]
        );
    }

    #[test]
    fn terminate_request_rejects_zero_truncated_and_trailing_payloads() {
        assert!(decode_terminate_request(&[0; 12]).is_err());
        assert!(decode_terminate_request(&[1; 11]).is_err());
        assert!(decode_terminate_request(&[1; 13]).is_err());
    }

    #[test]
    fn inspection_payload_rejects_field_breaking_values_and_oversize_output() {
        let inspection = sample_inspection("bad\nname");
        assert!(encode_inspection_payload(&inspection, 4096).is_err());

        let oversized = sample_inspection(&"x".repeat(4096));
        assert!(encode_inspection_payload(&oversized, 4096).is_err());
    }

    #[test]
    fn inspection_payload_round_trips_allowed_fields() {
        let inspection = sample_inspection("demo.exe");
        let payload =
            encode_inspection_payload(&inspection, 4096).expect("inspection should encode");
        assert_eq!(
            decode_inspection_payload(&payload).expect("inspection should decode"),
            inspection
        );
    }

    #[test]
    fn status_payload_round_trips_fixed_status_names() {
        for status in [
            ProcessStatus::Terminated,
            ProcessStatus::TerminatePending,
            ProcessStatus::TargetNotFound,
            ProcessStatus::TargetAccessDenied,
            ProcessStatus::PidReused,
            ProcessStatus::TargetNotOwned,
            ProcessStatus::TargetProtected,
            ProcessStatus::CallerNotElevated,
            ProcessStatus::CallerNotAdmin,
            ProcessStatus::AccessDenied,
        ] {
            assert_eq!(
                decode_status_payload(&encode_status_payload(status))
                    .expect("status should decode"),
                status
            );
        }
    }
}
