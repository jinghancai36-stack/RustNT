use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Capability {
    Ping,
    Identity,
    Capabilities,
    ProcessInspect,
    ProcessTerminate,
}

impl Capability {
    pub const fn all() -> &'static [Self; 5] {
        &[
            Self::Ping,
            Self::Identity,
            Self::Capabilities,
            Self::ProcessInspect,
            Self::ProcessTerminate,
        ]
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Ping => "ping",
            Self::Identity => "identity",
            Self::Capabilities => "capabilities",
            Self::ProcessInspect => "process_inspect",
            Self::ProcessTerminate => "process_terminate",
        }
    }

    pub const fn is_destructive(self) -> bool {
        matches!(self, Self::ProcessTerminate)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestContext {
    pub request_id: u64,
    pub capability: Capability,
    pub client_sid: Option<String>,
    pub caller_elevated: bool,
    pub caller_administrator: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthorizationRejection {
    MissingClientSid,
    CallerNotElevated,
    CallerNotAdmin,
}

pub fn authorize(context: &RequestContext) -> Result<(), AuthorizationRejection> {
    if !context.capability.is_destructive() {
        return Ok(());
    }
    if context.client_sid.is_none() {
        return Err(AuthorizationRejection::MissingClientSid);
    }
    if !context.caller_elevated {
        return Err(AuthorizationRejection::CallerNotElevated);
    }
    if !context.caller_administrator {
        return Err(AuthorizationRejection::CallerNotAdmin);
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditOutcome {
    Succeeded,
    Rejected,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditReason {
    MissingClientSid,
    CallerNotElevated,
    CallerNotAdmin,
    TargetNotFound,
    TargetProtected,
    PidReused,
    TargetNotOwned,
    AccessDenied,
    MalformedPayload,
    RateLimited,
    WindowsFailure,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditEvent {
    pub request_id: u64,
    pub capability: Capability,
    pub outcome: AuditOutcome,
    pub reason: Option<AuditReason>,
    pub target_pid: Option<u32>,
    pub client_sid: Option<String>,
}

impl AuditEvent {
    pub fn success(
        request_id: u64,
        capability: Capability,
        target_pid: Option<u32>,
        client_sid: Option<String>,
    ) -> Self {
        Self {
            request_id,
            capability,
            outcome: AuditOutcome::Succeeded,
            reason: None,
            target_pid,
            client_sid,
        }
    }

    pub fn rejected(
        request_id: u64,
        capability: Capability,
        reason: AuditReason,
        target_pid: Option<u32>,
        client_sid: Option<String>,
    ) -> Self {
        Self {
            request_id,
            capability,
            outcome: AuditOutcome::Rejected,
            reason: Some(reason),
            target_pid,
            client_sid,
        }
    }

    pub fn failed(
        request_id: u64,
        capability: Capability,
        reason: AuditReason,
        target_pid: Option<u32>,
        client_sid: Option<String>,
    ) -> Self {
        Self {
            request_id,
            capability,
            outcome: AuditOutcome::Failed,
            reason: Some(reason),
            target_pid,
            client_sid,
        }
    }
}

pub trait AuditSink {
    fn record(&self, event: AuditEvent);
}

#[derive(Clone)]
pub struct MemoryAuditSink {
    events: Arc<Mutex<VecDeque<AuditEvent>>>,
    capacity: usize,
}

impl MemoryAuditSink {
    pub fn new(capacity: usize) -> Self {
        Self {
            events: Arc::new(Mutex::new(VecDeque::new())),
            capacity,
        }
    }

    pub fn snapshot(&self) -> Vec<AuditEvent> {
        self.events
            .lock()
            .expect("audit sink mutex must not be poisoned")
            .iter()
            .cloned()
            .collect()
    }

    pub fn record(&self, event: AuditEvent) {
        if self.capacity == 0 {
            return;
        }
        let mut events = self
            .events
            .lock()
            .expect("audit sink mutex must not be poisoned");
        while events.len() >= self.capacity {
            events.pop_front();
        }
        events.push_back(event);
    }
}

impl AuditSink for MemoryAuditSink {
    fn record(&self, event: AuditEvent) {
        MemoryAuditSink::record(self, event);
    }
}

#[derive(Debug)]
pub struct RequestRateLimiter {
    max_requests: usize,
    window: Duration,
    requests: HashMap<String, VecDeque<Instant>>,
}

impl RequestRateLimiter {
    pub fn new(max_requests: usize, window: Duration) -> Self {
        Self {
            max_requests,
            window,
            requests: HashMap::new(),
        }
    }

    pub fn allow(&mut self, subject: &str) -> bool {
        self.allow_at(subject, Instant::now())
    }

    pub fn allow_at(&mut self, subject: &str, now: Instant) -> bool {
        let requests = self.requests.entry(subject.to_string()).or_default();
        while requests
            .front()
            .is_some_and(|started| now.saturating_duration_since(*started) >= self.window)
        {
            requests.pop_front();
        }
        if requests.len() >= self.max_requests {
            return false;
        }
        requests.push_back(now);
        true
    }

    pub fn clear_expired(&mut self, now: Instant) {
        self.requests.retain(|_, requests| {
            while requests
                .front()
                .is_some_and(|started| now.saturating_duration_since(*started) >= self.window)
            {
                requests.pop_front();
            }
            !requests.is_empty()
        });
    }
}

#[cfg(test)]
mod tests {
    use super::{
        authorize, AuditEvent, AuthorizationRejection, Capability, MemoryAuditSink, RequestContext,
        RequestRateLimiter,
    };
    use std::time::{Duration, Instant};

    #[test]
    fn fixed_capabilities_have_stable_names() {
        assert_eq!(
            Capability::all()
                .iter()
                .map(|capability| capability.name())
                .collect::<Vec<_>>(),
            vec![
                "ping",
                "identity",
                "capabilities",
                "process_inspect",
                "process_terminate",
            ]
        );
    }

    #[test]
    fn termination_authorization_requires_elevated_local_admin() {
        let context = RequestContext {
            request_id: 1,
            capability: Capability::ProcessTerminate,
            client_sid: Some("S-1-5-21-user".to_string()),
            caller_elevated: false,
            caller_administrator: true,
        };
        assert_eq!(
            authorize(&context),
            Err(AuthorizationRejection::CallerNotElevated)
        );
    }

    #[test]
    fn memory_audit_sink_keeps_only_the_newest_events() {
        let sink = MemoryAuditSink::new(2);
        sink.record(AuditEvent::success(1, Capability::Ping, None, None));
        sink.record(AuditEvent::success(2, Capability::Ping, None, None));
        sink.record(AuditEvent::success(3, Capability::Ping, None, None));
        assert_eq!(
            sink.snapshot()
                .iter()
                .map(|event| event.request_id)
                .collect::<Vec<_>>(),
            vec![2, 3]
        );
    }

    #[test]
    fn rate_limiter_rejects_burst_and_allows_after_window() {
        let start = Instant::now();
        let mut limiter = RequestRateLimiter::new(2, Duration::from_secs(10));
        assert!(limiter.allow_at("S-1-5-21-user", start));
        assert!(limiter.allow_at("S-1-5-21-user", start + Duration::from_secs(1)));
        assert!(!limiter.allow_at("S-1-5-21-user", start + Duration::from_secs(2)));
        assert!(limiter.allow_at("S-1-5-21-user", start + Duration::from_secs(11)));
    }
}
