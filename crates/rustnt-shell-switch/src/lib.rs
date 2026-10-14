use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ShellOperation {
    PrepareSwitch,
    Switch,
    Restore,
    ShowRecovery,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ShellState {
    Explorer,
    RustNt,
    External,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum TransactionState {
    Prepared,
    BackedUp,
    ShellWritten,
    ReadBackVerified,
    Committed,
    Failed,
    RestoreRequested,
    Restored,
}

impl TransactionState {
    pub const fn can_transition_to(self, next: Self) -> bool {
        use TransactionState::*;

        matches!(
            (self, next),
            (Prepared, BackedUp)
                | (BackedUp, ShellWritten)
                | (ShellWritten, ReadBackVerified)
                | (ReadBackVerified, Committed)
                | (Committed, RestoreRequested)
                | (RestoreRequested, Restored)
                | (Prepared, Failed)
                | (BackedUp, Failed)
                | (ShellWritten, Failed)
                | (ReadBackVerified, Failed)
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum RegistryType {
    String,
    ExpandString,
    Other(u32),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegistryValue {
    pub value_type: RegistryType,
    pub raw: Vec<u8>,
}

impl RegistryValue {
    pub fn new(value_type: RegistryType, raw: Vec<u8>) -> Self {
        Self { value_type, raw }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Nonce(pub [u8; 16]);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RequestContext {
    pub pid: u32,
    pub timestamp: u64,
    pub nonce: Nonce,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatedInstall {
    root: PathBuf,
    helper: PathBuf,
    bootstrap: PathBuf,
    gui: PathBuf,
}

impl ValidatedInstall {
    pub fn root(&self) -> &std::path::Path {
        &self.root
    }

    pub fn helper(&self) -> &std::path::Path {
        &self.helper
    }

    pub fn bootstrap(&self) -> &std::path::Path {
        &self.bootstrap
    }

    pub fn gui(&self) -> &std::path::Path {
        &self.gui
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum SwitchErrorKind {
    InvalidArguments,
    UnsupportedRegistryType,
    InvalidRegistryString,
    InvalidInstall,
    InvalidManifest,
    UnsafeAcl,
    InvalidTransaction,
    BackupUnavailable,
    ExternalModification,
    LockTimeout,
    RegistryFailure,
    StorageFailure,
    UacCancelled,
    LaunchFailure,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SwitchError {
    pub kind: SwitchErrorKind,
    pub win32_code: Option<u32>,
}

impl SwitchError {
    pub const fn kind(&self) -> SwitchErrorKind {
        self.kind
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ShellValueMetadata {
    pub value_type: RegistryType,
    pub byte_len: usize,
    pub sha256: [u8; 32],
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum RegistrySnapshot {
    Missing,
    Present(ShellValueMetadata),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum AuditEventKind {
    Prepared,
    BackupVerified,
    ShellWriteRequested,
    ReadBackVerified,
    Committed,
    Failed,
    RestoreRequested,
    Restored,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AuditEvent {
    pub kind: AuditEventKind,
    pub timestamp: u64,
    pub state: TransactionState,
    pub error: Option<SwitchErrorKind>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ShellTransaction {
    pub format_version: u32,
    pub nonce: Nonce,
    pub state: TransactionState,
    pub created_at: u64,
    pub request_pid: u32,
    pub shell_before: RegistrySnapshot,
    pub userinit_before: RegistrySnapshot,
    pub shell_target: ShellValueMetadata,
    pub events: Vec<AuditEvent>,
    pub error: Option<SwitchErrorKind>,
}

#[cfg(test)]
mod tests {
    use super::TransactionState;

    #[test]
    fn transaction_state_rejects_skipping_backup_before_shell_write() {
        assert!(!TransactionState::Prepared.can_transition_to(TransactionState::ShellWritten));
        assert!(TransactionState::Prepared.can_transition_to(TransactionState::BackedUp));
    }

    #[test]
    fn transaction_state_allows_only_declared_forward_and_recovery_edges() {
        use TransactionState::*;

        let allowed = [
            (Prepared, BackedUp),
            (BackedUp, ShellWritten),
            (ShellWritten, ReadBackVerified),
            (ReadBackVerified, Committed),
            (Committed, RestoreRequested),
            (RestoreRequested, Restored),
            (Prepared, Failed),
            (BackedUp, Failed),
            (ShellWritten, Failed),
            (ReadBackVerified, Failed),
        ];
        let states = [
            Prepared,
            BackedUp,
            ShellWritten,
            ReadBackVerified,
            Committed,
            Failed,
            RestoreRequested,
            Restored,
        ];

        for from in states {
            for to in states {
                assert_eq!(
                    from.can_transition_to(to),
                    allowed.contains(&(from, to)),
                    "{from:?} -> {to:?}"
                );
            }
        }
    }

    #[test]
    fn failed_state_requires_recovery_validation_before_restore_request() {
        assert!(!TransactionState::Failed.can_transition_to(TransactionState::RestoreRequested));
    }
}
