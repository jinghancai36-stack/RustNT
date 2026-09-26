use std::collections::VecDeque;

const MAX_NOTIFICATIONS: usize = 32;
const MAX_MESSAGE_LENGTH: usize = 512;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NotificationKind {
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Notification {
    pub kind: NotificationKind,
    pub message: String,
    pub read: bool,
}

#[derive(Debug, Default)]
pub struct NotificationCenter {
    notifications: VecDeque<Notification>,
}

impl NotificationCenter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, kind: NotificationKind, message: impl Into<String>) {
        let message = message
            .into()
            .chars()
            .filter(|character| *character != '\r' && *character != '\n')
            .take(MAX_MESSAGE_LENGTH)
            .collect();
        self.notifications.push_back(Notification {
            kind,
            message,
            read: false,
        });
        while self.notifications.len() > MAX_NOTIFICATIONS {
            self.notifications.pop_front();
        }
    }

    pub fn notifications(&self) -> &VecDeque<Notification> {
        &self.notifications
    }

    pub fn unread_count(&self) -> usize {
        self.notifications
            .iter()
            .filter(|notification| !notification.read)
            .count()
    }

    pub fn mark_all_read(&mut self) {
        for notification in &mut self.notifications {
            notification.read = true;
        }
    }

    pub fn mark_read(&mut self, index: usize) {
        if let Some(notification) = self.notifications.get_mut(index) {
            notification.read = true;
        }
    }

    pub fn clear(&mut self) {
        self.notifications.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::{NotificationCenter, NotificationKind};

    #[test]
    fn new_notifications_are_unread() {
        let mut center = NotificationCenter::new();

        center.push(NotificationKind::Info, "ready");

        assert_eq!(center.notifications()[0].message, "ready");
        assert!(!center.notifications()[0].read);
        assert_eq!(center.unread_count(), 1);
    }

    #[test]
    fn read_operations_are_idempotent_and_invalid_indices_are_safe() {
        let mut center = NotificationCenter::new();
        center.push(NotificationKind::Success, "saved");

        center.mark_read(0);
        center.mark_read(0);
        center.mark_read(1);
        center.mark_all_read();
        center.mark_all_read();

        assert_eq!(center.unread_count(), 0);
        assert!(center.notifications()[0].read);
    }

    #[test]
    fn clear_is_idempotent() {
        let mut center = NotificationCenter::new();
        center.push(NotificationKind::Warning, "disk space");

        center.clear();
        center.clear();

        assert!(center.notifications().is_empty());
        assert_eq!(center.unread_count(), 0);
    }

    #[test]
    fn retains_only_the_newest_32_notifications() {
        let mut center = NotificationCenter::new();

        for index in 0..33 {
            center.push(NotificationKind::Info, index.to_string());
        }

        assert_eq!(center.notifications().len(), 32);
        assert_eq!(center.notifications().front().unwrap().message, "1");
        assert_eq!(center.notifications().back().unwrap().message, "32");
    }

    #[test]
    fn removes_carriage_returns_and_newlines() {
        let mut center = NotificationCenter::new();

        center.push(NotificationKind::Error, "line 1\r\nline 2\nline 3\r");

        assert_eq!(center.notifications()[0].message, "line 1line 2line 3");
    }

    #[test]
    fn truncates_messages_to_512_unicode_scalar_values() {
        let mut center = NotificationCenter::new();
        let message = format!("{}終", "a".repeat(512));

        center.push(NotificationKind::Info, message);

        let stored = &center.notifications()[0].message;
        assert_eq!(stored.chars().count(), 512);
        assert!(!stored.contains('終'));
    }
}
