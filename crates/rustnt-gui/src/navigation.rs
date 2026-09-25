#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PageId {
    Overview,
    SystemMonitor,
    WindowsSessions,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NavigationState {
    pub current_page: PageId,
    pub switcher_open: bool,
}

impl Default for NavigationState {
    fn default() -> Self {
        Self {
            current_page: PageId::Overview,
            switcher_open: false,
        }
    }
}

pub fn pages() -> &'static [PageId] {
    &[
        PageId::Overview,
        PageId::SystemMonitor,
        PageId::WindowsSessions,
    ]
}

pub fn page_title(page: PageId) -> &'static str {
    match page {
        PageId::Overview => "Overview",
        PageId::SystemMonitor => "System Monitor",
        PageId::WindowsSessions => "Windows Sessions",
    }
}

pub fn page_label(page: PageId) -> &'static str {
    match page {
        PageId::Overview => "Overview",
        PageId::SystemMonitor => "System",
        PageId::WindowsSessions => "Sessions",
    }
}

pub fn page_for_shortcut(ctrl: bool, key: eframe::egui::Key) -> Option<PageId> {
    if !ctrl {
        return None;
    }

    match key {
        eframe::egui::Key::Num1 => Some(PageId::Overview),
        eframe::egui::Key::Num2 => Some(PageId::SystemMonitor),
        eframe::egui::Key::Num3 => Some(PageId::WindowsSessions),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{page_for_shortcut, page_label, page_title, pages, NavigationState, PageId};
    use eframe::egui::Key;

    #[test]
    fn pages_have_fixed_order() {
        assert_eq!(
            pages(),
            &[
                PageId::Overview,
                PageId::SystemMonitor,
                PageId::WindowsSessions
            ]
        );
    }

    #[test]
    fn pages_have_stable_labels_and_titles() {
        assert_eq!(page_label(PageId::Overview), "Overview");
        assert_eq!(page_title(PageId::Overview), "Overview");
        assert_eq!(page_label(PageId::SystemMonitor), "System");
        assert_eq!(page_title(PageId::SystemMonitor), "System Monitor");
        assert_eq!(page_label(PageId::WindowsSessions), "Sessions");
        assert_eq!(page_title(PageId::WindowsSessions), "Windows Sessions");
    }

    #[test]
    fn ctrl_number_shortcuts_select_pages_in_order() {
        assert_eq!(page_for_shortcut(true, Key::Num1), Some(PageId::Overview));
        assert_eq!(
            page_for_shortcut(true, Key::Num2),
            Some(PageId::SystemMonitor)
        );
        assert_eq!(
            page_for_shortcut(true, Key::Num3),
            Some(PageId::WindowsSessions)
        );
    }

    #[test]
    fn shortcuts_reject_non_ctrl_and_unknown_keys() {
        assert_eq!(page_for_shortcut(false, Key::Num1), None);
        assert_eq!(page_for_shortcut(false, Key::Num2), None);
        assert_eq!(page_for_shortcut(false, Key::Num3), None);
        assert_eq!(page_for_shortcut(true, Key::A), None);
    }

    #[test]
    fn default_state_selects_overview_with_switcher_closed() {
        let state = NavigationState::default();
        assert_eq!(state.current_page, PageId::Overview);
        assert!(!state.switcher_open);
    }
}
