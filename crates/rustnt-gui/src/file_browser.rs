use std::cmp::Ordering;
use std::path::PathBuf;

use rustnt_core::filesystem::{list_directory, DirectoryEntry, FileKind, FileSystemError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortField {
    Name,
    Kind,
    Size,
    Modified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDirection {
    Ascending,
    Descending,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BrowserView {
    pub path: String,
    pub entries: Vec<DirectoryEntry>,
    pub selected_path: Option<String>,
    pub last_error: Option<FileSystemError>,
    pub is_empty: bool,
    pub loaded: bool,
}

impl BrowserView {
    pub fn empty(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            entries: Vec::new(),
            selected_path: None,
            last_error: None,
            is_empty: true,
            loaded: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct FileBrowserState {
    current_path: String,
    entries: Vec<DirectoryEntry>,
    back_stack: Vec<String>,
    forward_stack: Vec<String>,
    filter: String,
    sort_field: SortField,
    sort_direction: SortDirection,
    selected_path: Option<String>,
    last_error: Option<FileSystemError>,
    loaded: bool,
}

impl FileBrowserState {
    pub fn new(path: impl Into<String>) -> Self {
        Self {
            current_path: path.into(),
            entries: Vec::new(),
            back_stack: Vec::new(),
            forward_stack: Vec::new(),
            filter: String::new(),
            sort_field: SortField::Name,
            sort_direction: SortDirection::Ascending,
            selected_path: None,
            last_error: None,
            loaded: false,
        }
    }

    pub fn current_path(&self) -> &str {
        &self.current_path
    }

    pub fn entries(&self) -> &[DirectoryEntry] {
        &self.entries
    }

    pub fn back_stack(&self) -> &[String] {
        &self.back_stack
    }

    pub fn forward_stack(&self) -> &[String] {
        &self.forward_stack
    }

    pub fn filter(&self) -> &str {
        &self.filter
    }

    pub fn sort_field(&self) -> SortField {
        self.sort_field
    }

    pub fn sort_direction(&self) -> SortDirection {
        self.sort_direction
    }

    pub fn selected_path(&self) -> Option<&str> {
        self.selected_path.as_deref()
    }

    pub fn last_error(&self) -> Option<&FileSystemError> {
        self.last_error.as_ref()
    }

    pub fn set_filter(&mut self, filter: impl Into<String>) {
        self.filter = filter.into();
    }

    pub fn set_sort_field(&mut self, sort_field: SortField) {
        self.sort_field = sort_field;
    }

    pub fn set_sort_direction(&mut self, sort_direction: SortDirection) {
        self.sort_direction = sort_direction;
    }

    pub fn set_selected_path(&mut self, selected_path: Option<String>) {
        self.selected_path = selected_path;
    }

    pub fn load(&mut self) -> Result<(), FileSystemError> {
        self.load_with(list_directory)
    }

    pub fn load_with<F>(&mut self, loader: F) -> Result<(), FileSystemError>
    where
        F: FnOnce(&str) -> Result<Vec<DirectoryEntry>, FileSystemError>,
    {
        let path = self.current_path.clone();
        match loader(&path) {
            Ok(entries) => {
                self.apply_loaded(path, entries);
                Ok(())
            }
            Err(error) => {
                self.last_error = Some(error.clone());
                Err(error)
            }
        }
    }

    pub fn navigate_to<F>(
        &mut self,
        path: impl Into<String>,
        loader: F,
    ) -> Result<(), FileSystemError>
    where
        F: FnOnce(&str) -> Result<Vec<DirectoryEntry>, FileSystemError>,
    {
        let path = path.into();
        let entries = match loader(&path) {
            Ok(entries) => entries,
            Err(error) => {
                self.last_error = Some(error.clone());
                return Err(error);
            }
        };
        if path != self.current_path {
            self.back_stack.push(self.current_path.clone());
            self.forward_stack.clear();
        }
        self.apply_loaded(path, entries);
        Ok(())
    }

    pub fn enter_directory<F>(
        &mut self,
        path: impl Into<String>,
        loader: F,
    ) -> Result<(), FileSystemError>
    where
        F: FnOnce(&str) -> Result<Vec<DirectoryEntry>, FileSystemError>,
    {
        self.navigate_to(path, loader)
    }

    pub fn parent<F>(&mut self, loader: F) -> Result<bool, FileSystemError>
    where
        F: FnOnce(&str) -> Result<Vec<DirectoryEntry>, FileSystemError>,
    {
        match Self::parent_path(&self.current_path) {
            Some(path) => {
                self.navigate_to(path, loader)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    pub fn back<F>(&mut self, loader: F) -> Result<bool, FileSystemError>
    where
        F: FnOnce(&str) -> Result<Vec<DirectoryEntry>, FileSystemError>,
    {
        let Some(path) = self.back_stack.last().cloned() else {
            return Ok(false);
        };
        let entries = match loader(&path) {
            Ok(entries) => entries,
            Err(error) => {
                self.last_error = Some(error.clone());
                return Err(error);
            }
        };
        let previous = self.current_path.clone();
        self.back_stack.pop();
        self.forward_stack.push(previous);
        self.apply_loaded(path, entries);
        Ok(true)
    }

    pub fn forward<F>(&mut self, loader: F) -> Result<bool, FileSystemError>
    where
        F: FnOnce(&str) -> Result<Vec<DirectoryEntry>, FileSystemError>,
    {
        let Some(path) = self.forward_stack.last().cloned() else {
            return Ok(false);
        };
        let entries = match loader(&path) {
            Ok(entries) => entries,
            Err(error) => {
                self.last_error = Some(error.clone());
                return Err(error);
            }
        };
        let previous = self.current_path.clone();
        self.forward_stack.pop();
        self.back_stack.push(previous);
        self.apply_loaded(path, entries);
        Ok(true)
    }

    pub fn refresh<F>(&mut self, loader: F) -> Result<(), FileSystemError>
    where
        F: FnOnce(&str) -> Result<Vec<DirectoryEntry>, FileSystemError>,
    {
        self.load_with(loader)
    }

    pub fn view(&self) -> BrowserView {
        let entries = filter_and_sort_entries(
            &self.entries,
            &self.filter,
            self.sort_field,
            self.sort_direction,
        );
        BrowserView {
            path: self.current_path.clone(),
            is_empty: entries.is_empty(),
            entries,
            selected_path: self.selected_path.clone(),
            last_error: self.last_error.clone(),
            loaded: self.loaded,
        }
    }

    pub fn parent_path(path: &str) -> Option<String> {
        let trimmed = path.trim_end_matches(['\\', '/']);
        if trimmed.is_empty() || is_drive_root(path) || is_unc_share_root(trimmed) {
            return None;
        }
        let separator = trimmed.rfind(['\\', '/'])?;
        let parent = &trimmed[..=separator];
        if parent.len() == 2 && parent.as_bytes()[1] == b':' {
            Some(format!("{parent}\\"))
        } else {
            Some(parent.to_owned())
        }
    }

    fn apply_loaded(&mut self, path: String, entries: Vec<DirectoryEntry>) {
        self.current_path = path;
        self.entries = entries;
        self.selected_path = None;
        self.last_error = None;
        self.loaded = true;
    }
}

impl Default for FileBrowserState {
    fn default() -> Self {
        let path = std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from(r"C:\"))
            .to_string_lossy()
            .into_owned();
        Self::new(path)
    }
}

pub fn entry_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

pub fn filter_and_sort_entries(
    entries: &[DirectoryEntry],
    filter: &str,
    sort_field: SortField,
    sort_direction: SortDirection,
) -> Vec<DirectoryEntry> {
    let query = filter.to_lowercase();
    let mut visible: Vec<DirectoryEntry> = entries
        .iter()
        .filter(|entry| {
            entry_name(&entry.metadata.path)
                .to_lowercase()
                .contains(&query)
        })
        .cloned()
        .collect();
    visible.sort_by(|left, right| compare_entries(left, right, sort_field, sort_direction));
    visible
}

fn compare_entries(
    left: &DirectoryEntry,
    right: &DirectoryEntry,
    sort_field: SortField,
    sort_direction: SortDirection,
) -> Ordering {
    let left_name = entry_name(&left.metadata.path).to_lowercase();
    let right_name = entry_name(&right.metadata.path).to_lowercase();
    let primary = if sort_field == SortField::Name && sort_direction == SortDirection::Ascending {
        kind_rank(&left.metadata.kind)
            .cmp(&kind_rank(&right.metadata.kind))
            .then_with(|| left_name.cmp(&right_name))
    } else {
        compare_primary(left, right, &left_name, &right_name, sort_field)
    };
    let primary = match sort_direction {
        SortDirection::Ascending => primary,
        SortDirection::Descending => primary.reverse(),
    };
    primary
        .then_with(|| left_name.cmp(&right_name))
        .then_with(|| {
            left.metadata
                .path
                .to_lowercase()
                .cmp(&right.metadata.path.to_lowercase())
        })
        .then_with(|| left.metadata.path.cmp(&right.metadata.path))
}

fn compare_primary(
    left: &DirectoryEntry,
    right: &DirectoryEntry,
    left_name: &str,
    right_name: &str,
    sort_field: SortField,
) -> Ordering {
    match sort_field {
        SortField::Name => left_name.cmp(right_name),
        SortField::Kind => kind_rank(&left.metadata.kind).cmp(&kind_rank(&right.metadata.kind)),
        SortField::Size => left.metadata.size_bytes.cmp(&right.metadata.size_bytes),
        SortField::Modified => left.metadata.modified.cmp(&right.metadata.modified),
    }
}

fn kind_rank(kind: &FileKind) -> u8 {
    match kind {
        FileKind::Directory => 0,
        FileKind::ReparsePoint => 1,
        FileKind::File => 2,
        FileKind::Other => 3,
    }
}

fn is_drive_root(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() == 3 && bytes[1] == b':' && matches!(bytes[2], b'\\' | b'/')
}

fn is_unc_share_root(path: &str) -> bool {
    let trimmed = path.trim_end_matches(['\\', '/']);
    let bytes = trimmed.as_bytes();
    if bytes.len() < 2 || !matches!(bytes[0], b'\\' | b'/') || !matches!(bytes[1], b'\\' | b'/') {
        return false;
    }
    trimmed
        .trim_start_matches(['\\', '/'])
        .split(['\\', '/'])
        .filter(|part| !part.is_empty())
        .count()
        == 2
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::time::{Duration, UNIX_EPOCH};

    use rustnt_core::filesystem::{DirectoryEntry, FileKind, FileMetadata, FileSystemError};

    use super::{filter_and_sort_entries, BrowserView, FileBrowserState, SortDirection, SortField};

    fn metadata(path: &str, kind: FileKind, size: Option<u64>, modified: u64) -> FileMetadata {
        FileMetadata {
            path: path.to_owned(),
            kind,
            size_bytes: size,
            created: None,
            modified: Some(UNIX_EPOCH + Duration::from_secs(modified)),
            accessed: None,
            attributes: 0,
            is_reparse_point: false,
        }
    }

    fn entry(path: &str, kind: FileKind, size: Option<u64>, modified: u64) -> DirectoryEntry {
        DirectoryEntry {
            metadata: metadata(path, kind, size, modified),
        }
    }

    fn fixture_entries() -> Vec<DirectoryEntry> {
        vec![
            entry(r"C:\root\Zoo.txt", FileKind::File, Some(30), 3),
            entry(r"C:\root\alpha", FileKind::Directory, None, 2),
            entry(r"C:\root\Beta.txt", FileKind::File, Some(10), 1),
        ]
    }

    fn loader_for(
        directories: HashMap<String, Result<Vec<DirectoryEntry>, FileSystemError>>,
    ) -> impl Fn(&str) -> Result<Vec<DirectoryEntry>, FileSystemError> {
        move |path| {
            directories
                .get(path)
                .cloned()
                .unwrap_or_else(|| Err(FileSystemError::NotFound(path.to_owned())))
        }
    }

    #[test]
    fn default_state_starts_at_the_requested_path_without_loaded_entries() {
        let state = FileBrowserState::new(r"C:\root");

        assert_eq!(state.current_path(), r"C:\root");
        assert!(state.entries().is_empty());
        assert!(state.back_stack().is_empty());
        assert!(state.forward_stack().is_empty());
        assert_eq!(state.filter(), "");
        assert_eq!(state.sort_field(), SortField::Name);
        assert_eq!(state.sort_direction(), SortDirection::Ascending);
        assert!(state.selected_path().is_none());
        assert!(state.last_error().is_none());
    }

    #[test]
    fn successful_load_replaces_entries_and_clears_error() {
        let path = r"C:\root".to_owned();
        let mut directories = HashMap::new();
        directories.insert(path.clone(), Ok(fixture_entries()));
        let mut state = FileBrowserState::new(path.clone());
        state.set_filter("stale");

        state
            .load_with(loader_for(directories))
            .expect("fixture directory should load");

        assert_eq!(state.current_path(), path);
        assert_eq!(state.entries(), fixture_entries().as_slice());
        assert!(state.last_error().is_none());
        assert_eq!(state.view().entries.len(), 0);
    }

    #[test]
    fn successful_load_after_failure_clears_the_previous_error() {
        let root = r"C:\root".to_owned();
        let mut state = FileBrowserState::new(root.clone());
        let error = state
            .load_with(|_| Err(FileSystemError::AccessDenied("denied".to_owned())))
            .expect_err("first load should fail");
        assert_eq!(state.last_error(), Some(&error));

        state
            .load_with(|_| Ok(fixture_entries()))
            .expect("second load should succeed");

        assert_eq!(state.current_path(), root);
        assert!(state.last_error().is_none());
        assert_eq!(state.entries(), fixture_entries().as_slice());
    }

    #[test]
    fn failed_load_preserves_previous_directory_entries_and_history() {
        let root = r"C:\root".to_owned();
        let child = r"C:\root\child".to_owned();
        let mut directories = HashMap::new();
        directories.insert(root.clone(), Ok(fixture_entries()));
        directories.insert(
            child,
            Err(FileSystemError::AccessDenied("denied".to_owned())),
        );
        let mut state = FileBrowserState::new(root.clone());
        state
            .load_with(loader_for(directories.clone()))
            .expect("root should load");

        let before_entries = state.entries().to_vec();
        let before_back = state.back_stack().to_vec();
        let error = state
            .navigate_to(r"C:\root\child", loader_for(directories))
            .expect_err("child should fail");

        assert!(matches!(error, FileSystemError::AccessDenied(_)));
        assert_eq!(state.current_path(), root);
        assert_eq!(state.entries(), before_entries.as_slice());
        assert_eq!(state.back_stack(), before_back.as_slice());
        assert!(state.forward_stack().is_empty());
        assert!(state.last_error().is_some());
    }

    #[test]
    fn entering_a_child_pushes_back_and_clears_forward_history() {
        let root = r"C:\root".to_owned();
        let child = r"C:\root\child".to_owned();
        let sibling = r"C:\root\sibling".to_owned();
        let mut directories = HashMap::new();
        directories.insert(root.clone(), Ok(fixture_entries()));
        directories.insert(child.clone(), Ok(Vec::new()));
        directories.insert(sibling.clone(), Ok(Vec::new()));
        let mut state = FileBrowserState::new(root.clone());
        state
            .load_with(loader_for(directories.clone()))
            .expect("root should load");
        state
            .navigate_to(sibling, loader_for(directories.clone()))
            .expect("sibling should load");
        state
            .back(loader_for(directories.clone()))
            .expect("back should load");
        assert_eq!(state.forward_stack(), &[r"C:\root\sibling".to_owned()]);

        state
            .enter_directory(child.clone(), loader_for(directories))
            .expect("child should load");

        assert_eq!(state.current_path(), child);
        assert_eq!(state.back_stack(), &[root]);
        assert!(state.forward_stack().is_empty());
    }

    #[test]
    fn parent_navigation_stops_at_drive_and_unc_share_roots() {
        assert_eq!(FileBrowserState::parent_path(r"C:\"), None);
        assert_eq!(
            FileBrowserState::parent_path(r"C:\folder"),
            Some(r"C:\".to_owned())
        );
        assert_eq!(FileBrowserState::parent_path(r"\\server\share\"), None);
        assert_eq!(
            FileBrowserState::parent_path(r"\\server\share\folder"),
            Some(r"\\server\share\".to_owned())
        );
        assert_eq!(FileBrowserState::parent_path("//server/share/"), None);
        assert_eq!(
            FileBrowserState::parent_path("//server/share/folder"),
            Some("//server/share/".to_owned())
        );
    }

    #[test]
    fn back_and_forward_update_stacks_only_after_successful_loads() {
        let root = r"C:\root".to_owned();
        let child = r"C:\root\child".to_owned();
        let mut directories = HashMap::new();
        directories.insert(root.clone(), Ok(fixture_entries()));
        directories.insert(child.clone(), Ok(Vec::new()));
        let mut state = FileBrowserState::new(root.clone());
        state
            .navigate_to(child.clone(), loader_for(directories.clone()))
            .expect("child should load");
        state
            .back(loader_for(directories.clone()))
            .expect("back should load");
        assert_eq!(state.current_path(), root);
        assert!(state.back_stack().is_empty());
        assert_eq!(state.forward_stack(), &[child.clone()]);
        state
            .forward(loader_for(directories))
            .expect("forward should load");
        assert_eq!(state.current_path(), child);
        assert!(state.back_stack().is_empty() == false);
        assert!(state.forward_stack().is_empty());
    }

    #[test]
    fn refresh_reloads_current_path_without_changing_history() {
        let root = r"C:\root".to_owned();
        let mut directories = HashMap::new();
        directories.insert(root.clone(), Ok(fixture_entries()));
        let mut state = FileBrowserState::new(root.clone());
        state
            .load_with(loader_for(directories.clone()))
            .expect("root should load");
        state.set_selected_path(Some(r"C:\root\alpha".to_owned()));

        state
            .refresh(loader_for(directories))
            .expect("refresh should load");

        assert_eq!(state.current_path(), root);
        assert!(state.back_stack().is_empty());
        assert!(state.forward_stack().is_empty());
        assert!(state.selected_path().is_none());
        assert!(state.last_error().is_none());
    }

    #[test]
    fn filter_is_case_insensitive_and_matches_names_only() {
        let entries = fixture_entries();
        let filtered =
            filter_and_sort_entries(&entries, "ALp", SortField::Name, SortDirection::Ascending);

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].metadata.path, r"C:\root\alpha");
    }

    #[test]
    fn default_name_sort_places_directories_before_files() {
        let sorted = filter_and_sort_entries(
            &fixture_entries(),
            "",
            SortField::Name,
            SortDirection::Ascending,
        );

        assert_eq!(
            sorted
                .iter()
                .map(|entry| entry.metadata.path.as_str())
                .collect::<Vec<_>>(),
            vec![r"C:\root\alpha", r"C:\root\Beta.txt", r"C:\root\Zoo.txt"]
        );
    }

    #[test]
    fn every_sort_field_supports_both_directions() {
        let entries = fixture_entries();
        let cases = [
            (
                SortField::Name,
                vec![r"C:\root\alpha", r"C:\root\Beta.txt", r"C:\root\Zoo.txt"],
                vec![r"C:\root\Zoo.txt", r"C:\root\Beta.txt", r"C:\root\alpha"],
            ),
            (
                SortField::Kind,
                vec![r"C:\root\alpha", r"C:\root\Beta.txt", r"C:\root\Zoo.txt"],
                vec![r"C:\root\Beta.txt", r"C:\root\Zoo.txt", r"C:\root\alpha"],
            ),
            (
                SortField::Size,
                vec![r"C:\root\alpha", r"C:\root\Beta.txt", r"C:\root\Zoo.txt"],
                vec![r"C:\root\Zoo.txt", r"C:\root\Beta.txt", r"C:\root\alpha"],
            ),
            (
                SortField::Modified,
                vec![r"C:\root\Beta.txt", r"C:\root\alpha", r"C:\root\Zoo.txt"],
                vec![r"C:\root\Zoo.txt", r"C:\root\alpha", r"C:\root\Beta.txt"],
            ),
        ];
        for (field, expected_ascending, expected_descending) in cases {
            let ascending = filter_and_sort_entries(&entries, "", field, SortDirection::Ascending);
            let descending =
                filter_and_sort_entries(&entries, "", field, SortDirection::Descending);
            assert_eq!(
                ascending
                    .iter()
                    .map(|entry| entry.metadata.path.as_str())
                    .collect::<Vec<_>>(),
                expected_ascending
            );
            assert_eq!(
                descending
                    .iter()
                    .map(|entry| entry.metadata.path.as_str())
                    .collect::<Vec<_>>(),
                expected_descending
            );
        }
    }

    #[test]
    fn sort_ties_use_case_insensitive_name_then_path() {
        let entries = vec![
            entry(r"C:\root\same\b.txt", FileKind::File, Some(1), 1),
            entry(r"C:\root\same\A.txt", FileKind::File, Some(1), 1),
            entry(r"C:\root\same\a.txt", FileKind::File, Some(1), 1),
        ];
        let sorted =
            filter_and_sort_entries(&entries, "", SortField::Size, SortDirection::Ascending);

        assert_eq!(
            sorted
                .iter()
                .map(|entry| entry.metadata.path.as_str())
                .collect::<Vec<_>>(),
            vec![
                r"C:\root\same\A.txt",
                r"C:\root\same\a.txt",
                r"C:\root\same\b.txt",
            ]
        );
    }

    #[test]
    fn empty_directory_is_reported_by_the_view() {
        let state = FileBrowserState::new(r"C:\empty");

        assert_eq!(state.view(), BrowserView::empty(r"C:\empty"));
    }

    #[test]
    fn reparse_point_metadata_is_retained_in_loaded_and_visible_entries() {
        let mut reparse = entry(r"C:\root\junction", FileKind::ReparsePoint, None, 4);
        reparse.metadata.is_reparse_point = true;
        let root = r"C:\root".to_owned();
        let mut directories = HashMap::new();
        directories.insert(root.clone(), Ok(vec![reparse]));
        let mut state = FileBrowserState::new(root);
        state
            .load_with(loader_for(directories))
            .expect("root should load");

        assert!(state.entries()[0].metadata.is_reparse_point);
        assert!(state.view().entries[0].metadata.is_reparse_point);
    }
}
