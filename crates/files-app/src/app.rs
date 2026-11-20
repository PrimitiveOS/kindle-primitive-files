use std::{
    cell::RefCell,
    io,
    path::{Path, PathBuf},
    rc::Rc,
};

use primitive_app::{with_storage, App, Change};
use primitive_ui::{Canvas, Editor, Input, Key, Press};

use crate::{
    files::{EntryKind, FileEntry, FileStore},
    reader::{self, OpenRequest, Reader, Readers},
    ui::{Hit, Panel, Row, View},
};

enum NameAction {
    NewFolder,
    Rename(PathBuf),
}

enum Modal {
    None,
    Actions(FileEntry),
    Name { editor: Editor, action: NameAction },
    Delete(FileEntry),
}

struct Search {
    editor: Editor,
    keyboard: bool,
}

struct Transfer {
    path: PathBuf,
    moving: bool,
    label: String,
}

pub struct Files {
    root: PathBuf,
    store: FileStore,
    directory: PathBuf,
    directory_title: String,
    entries: Vec<FileEntry>,
    matching: Vec<usize>,
    rows: Vec<Row>,
    first: usize,
    modal: Modal,
    search: Option<Search>,
    transfer: Option<Transfer>,
    notice: Option<String>,
    readers: Readers,
    pending_open: Rc<RefCell<Option<OpenRequest>>>,
}

impl Files {
    pub fn new(root: PathBuf) -> io::Result<Self> {
        let (store, directory, entries, readers) = with_storage(|| {
            let store = FileStore::new(&root)?;
            let directory = store.initial_dir();
            let entries = store.list(&directory)?;
            Ok((store, directory, entries, Readers::installed(&root)))
        })?;
        let mut files = Self {
            root,
            store,
            directory_title: directory_title(&directory),
            directory,
            entries,
            matching: Vec::new(),
            rows: Vec::new(),
            first: 0,
            modal: Modal::None,
            search: None,
            transfer: None,
            notice: None,
            readers,
            pending_open: Rc::new(RefCell::new(None)),
        };
        files.update_rows();
        Ok(files)
    }

    pub fn pending_open(&self) -> Rc<RefCell<Option<OpenRequest>>> {
        Rc::clone(&self.pending_open)
    }

    pub fn view(&self) -> View<'_> {
        let panel = match &self.modal {
            Modal::None => self
                .search
                .as_ref()
                .map_or(Panel::None, |search| Panel::Search {
                    editor: &search.editor,
                    keyboard: search.keyboard,
                }),
            Modal::Actions(entry) => Panel::Actions {
                name: &entry.name,
                folder: entry.kind == EntryKind::Directory,
                readers: if entry.kind == EntryKind::File {
                    self.readers.for_path(&entry.path)
                } else {
                    Readers::default()
                },
            },
            Modal::Delete(entry) => Panel::Delete { name: &entry.name },
            Modal::Name { editor, action } => Panel::Name {
                title: match action {
                    NameAction::NewFolder => "New folder",
                    NameAction::Rename(_) => "Rename",
                },
                editor,
            },
        };
        View {
            directory: &self.directory_title,
            rows: &self.rows,
            first: self.first,
            can_go_up: !self.directory.as_os_str().is_empty(),
            can_edit: !self.directory.as_os_str().is_empty(),
            notice: self.notice.as_deref(),
            panel,
            clipboard: self
                .transfer
                .as_ref()
                .map(|transfer| transfer.label.as_str()),
        }
    }

    fn update_rows(&mut self) {
        let query = self
            .search
            .as_ref()
            .map(|search| search.editor.string())
            .unwrap_or_default()
            .trim()
            .to_lowercase();
        self.matching.clear();
        self.rows.clear();
        for (index, entry) in self.entries.iter().enumerate() {
            if !query.is_empty() && !entry.name.to_lowercase().contains(&query) {
                continue;
            }
            self.matching.push(index);
            self.rows.push(Row {
                detail: file_detail(entry),
                name: entry.name.clone(),
                folder: entry.kind == EntryKind::Directory,
            });
        }
        self.clamp_scroll();
    }

    fn clamp_scroll(&mut self) {
        self.first = self.first.min(self.view().max_first());
    }

    fn shown_entry(&self, index: usize) -> Option<FileEntry> {
        self.matching
            .get(index)
            .and_then(|index| self.entries.get(*index))
            .cloned()
    }

    fn open_directory(&mut self, directory: PathBuf) -> Change {
        match with_storage(|| self.store.list(&directory)) {
            Ok(entries) => {
                self.directory_title = directory_title(&directory);
                self.directory = directory;
                self.entries = entries;
                self.first = 0;
                self.modal = Modal::None;
                self.search = None;
                self.notice = None;
                self.update_rows();
                Change::Full
            }
            Err(error) => self.report_error("open this folder", &error),
        }
    }

    fn open_entry(&mut self, entry: FileEntry, reader: Option<Reader>) -> Change {
        if entry.kind == EntryKind::Directory {
            return self.open_directory(entry.path);
        }
        self.modal = Modal::None;
        let result = with_storage(|| {
            let path = self.store.openable_path(&entry.path)?;
            let reader = reader
                .or_else(|| self.readers.for_path(&entry.path).preferred())
                .ok_or_else(|| missing_reader(&entry.path))?;
            reader::check(reader, &path, &self.root)?;
            Ok(OpenRequest { reader, path })
        });
        match result {
            Ok(request) => {
                *self.pending_open.borrow_mut() = Some(request);
                Change::Exit
            }
            Err(error) => self.report_error("open this file", &error),
        }
    }

    fn report_error(&mut self, action: &str, error: &io::Error) -> Change {
        self.notice = Some(format!("Could not {action}. {error}"));
        Change::Partial
    }

    /// A write may succeed even when relisting fails, for example if a directory
    /// disappears. Report these outcomes separately and discard stale rows.
    fn change_files(
        &mut self,
        action: &str,
        success: &str,
        operation: impl FnOnce(&FileStore) -> io::Result<()>,
    ) -> bool {
        let result = with_storage(|| {
            operation(&self.store)?;
            Ok(self.store.list(&self.directory))
        });
        match result {
            Ok(refresh) => {
                self.modal = Modal::None;
                match refresh {
                    Ok(entries) => {
                        self.entries = entries;
                        self.notice = Some(success.into());
                    }
                    Err(error) => {
                        self.entries.clear();
                        self.notice =
                            Some(format!("{success} Could not refresh this folder. {error}"));
                    }
                }
                self.update_rows();
                true
            }
            Err(error) => {
                self.report_error(action, &error);
                false
            }
        }
    }

    fn submit_name(&mut self) -> Change {
        let Modal::Name { editor, action } = &self.modal else {
            return Change::None;
        };
        let name = editor.string();
        match action {
            NameAction::NewFolder => {
                let directory = self.directory.clone();
                self.change_files("create the folder", "Folder created.", |store| {
                    store.create_dir(&directory, &name).map(|_| ())
                });
            }
            NameAction::Rename(path) => {
                let path = path.clone();
                if self.change_files("rename the item", "Item renamed.", |store| {
                    store.rename(&path, &name).map(|_| ())
                }) {
                    self.clear_transfer_from(&path);
                }
            }
        }
        Change::Full
    }

    fn paste(&mut self) -> Change {
        let Some(transfer) = &self.transfer else {
            return Change::None;
        };
        let source = transfer.path.clone();
        let directory = self.directory.clone();
        let moving = transfer.moving;
        let (action, message) = if moving {
            ("move the item", "Moved here.")
        } else {
            ("copy the item", "Copied here.")
        };
        if self.change_files(action, message, |store| {
            if moving {
                store.move_entry(&source, &directory)
            } else {
                store.copy(&source, &directory)
            }
            .map(|_| ())
        }) {
            self.transfer = None;
            self.clamp_scroll();
        }
        Change::Partial
    }

    fn confirm_delete(&mut self) -> Change {
        let Modal::Delete(entry) = &self.modal else {
            return Change::None;
        };
        let path = entry.path.clone();
        self.modal = Modal::None;
        if self.change_files("delete the item", "Item deleted.", |store| {
            store.delete(&path)
        }) {
            self.clear_transfer_from(&path);
        }
        Change::Full
    }

    fn clear_transfer_from(&mut self, path: &Path) {
        if self
            .transfer
            .as_ref()
            .is_some_and(|transfer| transfer.path.starts_with(path))
        {
            self.transfer = None;
            self.clamp_scroll();
        }
    }

    fn key(&mut self, key: Key) -> Change {
        if let Modal::Name { editor, .. } = &mut self.modal {
            return match editor.press(key) {
                Press::Go => self.submit_name(),
                Press::Nothing => Change::None,
                Press::Edited => {
                    self.notice = None;
                    Change::Partial
                }
                _ => Change::Partial,
            };
        }
        let Some(search) = &mut self.search else {
            return Change::None;
        };
        match search.editor.press(key) {
            Press::Nothing => Change::None,
            Press::Go => {
                search.keyboard = false;
                self.clamp_scroll();
                Change::Full
            }
            Press::Edited => {
                self.first = 0;
                self.notice = None;
                self.update_rows();
                Change::Partial
            }
            _ => Change::Partial,
        }
    }

    fn cancel(&mut self) -> Change {
        if matches!(self.modal, Modal::None) {
            self.search = None;
            self.first = 0;
            self.update_rows();
        } else {
            self.modal = Modal::None;
        }
        self.notice = None;
        self.clamp_scroll();
        Change::Full
    }

    fn selected_action(&mut self, hit: Hit) -> Change {
        let Modal::Actions(entry) = &self.modal else {
            return Change::None;
        };
        let entry = entry.clone();
        match hit {
            Hit::OpenSelected => self.open_entry(entry, None),
            Hit::OpenIn(reader) => self.open_entry(entry, Some(reader)),
            Hit::Rename => {
                let mut editor = Editor::new(255);
                editor.text = entry.name.chars().collect();
                editor.caret = editor.text.len();
                self.modal = Modal::Name {
                    editor,
                    action: NameAction::Rename(entry.path),
                };
                Change::Full
            }
            Hit::Copy | Hit::Move => {
                let moving = hit == Hit::Move;
                let verb = if moving { "Move" } else { "Copy" };
                self.transfer = Some(Transfer {
                    path: entry.path,
                    moving,
                    label: format!("{verb} {}", entry.name),
                });
                self.modal = Modal::None;
                self.clamp_scroll();
                Change::Full
            }
            Hit::Delete => {
                self.modal = Modal::Delete(entry);
                Change::Full
            }
            _ => Change::None,
        }
    }

    fn respond(&mut self, hit: Hit) -> Change {
        match hit {
            Hit::None => Change::None,
            Hit::Exit => Change::Exit,
            Hit::Up => {
                let Some(parent) = self.directory.parent() else {
                    return Change::None;
                };
                self.open_directory(parent.to_owned())
            }
            Hit::Scroll(delta) => {
                let next = self
                    .first
                    .saturating_add_signed(delta)
                    .min(self.view().max_first());
                if next == self.first {
                    Change::None
                } else {
                    self.first = next;
                    Change::Partial
                }
            }
            Hit::Search => {
                let search = self.search.get_or_insert_with(|| Search {
                    editor: Editor::new(255),
                    keyboard: true,
                });
                search.keyboard = true;
                self.notice = None;
                self.clamp_scroll();
                Change::Full
            }
            Hit::NewFolder => {
                self.notice = None;
                self.modal = Modal::Name {
                    editor: Editor::new(255),
                    action: NameAction::NewFolder,
                };
                Change::Full
            }
            Hit::Paste => self.paste(),
            Hit::ClearClipboard => {
                self.transfer = None;
                self.clamp_scroll();
                Change::Partial
            }
            Hit::Open(index) => match self.shown_entry(index) {
                Some(entry) => self.open_entry(entry, None),
                None => Change::None,
            },
            Hit::Select(index) => {
                let Some(entry) = self.shown_entry(index) else {
                    return Change::None;
                };
                self.notice = None;
                self.modal = Modal::Actions(entry);
                Change::Full
            }
            Hit::OpenSelected
            | Hit::OpenIn(_)
            | Hit::Copy
            | Hit::Move
            | Hit::Rename
            | Hit::Delete => self.selected_action(hit),
            Hit::Confirm => self.confirm_delete(),
            Hit::Cancel => self.cancel(),
            Hit::Submit => self.submit_name(),
            Hit::Key(key) => self.key(key),
        }
    }
}

impl App for Files {
    fn draw(&self, canvas: &mut Canvas) {
        self.view().draw(canvas);
    }

    fn input(&mut self, input: Input) -> Change {
        let hit = self.view().hit(input);
        self.respond(hit)
    }

    fn storage_returned(&mut self) -> io::Result<()> {
        self.modal = Modal::None;
        let had_transfer = self.transfer.take().is_some();
        let mut directory = self.directory.clone();
        let result = with_storage(|| loop {
            match self.store.list(&directory) {
                Ok(entries) => break Ok((entries, Readers::installed(&self.root))),
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::NotFound
                            | io::ErrorKind::NotADirectory
                            | io::ErrorKind::PermissionDenied
                    ) && directory.pop() => {}
                Err(error) => break Err(error),
            }
        });
        match result {
            Ok((entries, readers)) => {
                self.readers = readers;
                if self.directory != directory {
                    self.directory = directory;
                    self.directory_title = directory_title(&self.directory);
                    self.search = None;
                    self.first = 0;
                    self.notice = Some("The previous folder is no longer available.".into());
                } else {
                    self.notice = had_transfer
                        .then(|| "Storage changed. Select the item to copy or move again.".into());
                }
                self.entries = entries;
                self.update_rows();
                Ok(())
            }
            Err(error) => {
                self.entries.clear();
                self.update_rows();
                self.report_error("reload Kindle storage", &error);
                Err(error)
            }
        }
    }
}

fn missing_reader(path: &Path) -> io::Error {
    let message = match (
        Reader::Viewer.supports(path),
        Reader::KoReader.supports(path),
        path.starts_with("documents") && Reader::PrimitiveReader.supports(path),
    ) {
        (true, true, true) => "Install Viewer, KOReader, or Reader to open this PDF.",
        (true, true, false) => "Install Viewer or KOReader to open this PDF.",
        (true, false, true) => "Install Viewer or Reader to open this image.",
        (true, false, false) => "Install Viewer to open this image.",
        (false, true, true) => "Install KOReader or Reader to open this document.",
        (false, true, false) => "Install KOReader to open this document.",
        (false, false, true) => "Install Reader to open this document.",
        (false, false, false) => "No supported reader can open this file type.",
    };
    io::Error::new(io::ErrorKind::Unsupported, message)
}

fn directory_title(directory: &Path) -> String {
    let mut parts = directory.iter();
    let Some(first) = parts.next() else {
        return "Kindle storage".into();
    };
    let mut title = if first.eq_ignore_ascii_case("documents") {
        "Documents".into()
    } else {
        first.to_string_lossy().into_owned()
    };
    for part in parts {
        title.push_str(" / ");
        title.push_str(&part.to_string_lossy());
    }
    title
}

fn file_detail(entry: &FileEntry) -> String {
    if entry.kind == EntryKind::Directory {
        return "Folder".into();
    }
    let kind = entry
        .path
        .extension()
        .and_then(|extension| extension.to_str())
        .filter(|extension| !extension.is_empty() && extension.len() <= 8)
        .map(str::to_uppercase)
        .unwrap_or_else(|| "File".into());
    let size = entry.size;
    let display_size = if size < 1024 {
        format!("{size} B")
    } else if size < 1024 * 1024 {
        format!("{:.1} KB", size as f64 / 1024.0)
    } else if size < 1024 * 1024 * 1024 {
        format!("{:.1} MB", size as f64 / 1_048_576.0)
    } else {
        format!("{:.1} GB", size as f64 / 1_073_741_824.0)
    };
    format!("{kind} · {display_size}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        sync::atomic::{AtomicUsize, Ordering},
    };

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let id = NEXT.fetch_add(1, Ordering::Relaxed);
            let root =
                std::env::temp_dir().join(format!("files-controller-{}-{id}", std::process::id()));
            fs::create_dir(&root).unwrap();
            fs::create_dir(root.join("documents")).unwrap();
            fs::create_dir(root.join("downloads")).unwrap();
            fs::write(root.join("documents/Alpha.txt"), b"first").unwrap();
            fs::write(root.join("documents/Beta.txt"), b"second").unwrap();
            Self(root)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn search_filters_the_visible_rows_and_copy_uses_the_selected_file() {
        let fixture = Fixture::new();
        let mut files = Files::new(fixture.0.clone()).unwrap();
        assert_eq!(files.view().directory, "Documents");
        files.respond(Hit::Search);
        files.respond(Hit::Key(Key::Char('b')));
        files.respond(Hit::Key(Key::Go));
        assert_eq!(files.view().rows.len(), 1);
        assert_eq!(files.view().rows[0].name, "Beta.txt");
        files.respond(Hit::Select(0));
        files.respond(Hit::Copy);
        files.respond(Hit::Up);
        assert_eq!(files.view().directory, "Kindle storage");
        files.respond(Hit::Open(1));
        files.respond(Hit::Paste);
        assert_eq!(
            fs::read(fixture.0.join("downloads/Beta.txt")).unwrap(),
            b"second"
        );
        assert!(fixture.0.join("documents/Beta.txt").exists());
        assert!(files.view().clipboard.is_none());
    }

    #[test]
    fn delete_requires_confirmation_and_usb_return_recovers_from_a_removed_folder() {
        let fixture = Fixture::new();
        let mut files = Files::new(fixture.0.clone()).unwrap();
        files.respond(Hit::Select(0));
        files.respond(Hit::Delete);
        assert!(fixture.0.join("documents/Alpha.txt").exists());
        files.respond(Hit::Cancel);
        assert!(fixture.0.join("documents/Alpha.txt").exists());
        files.respond(Hit::Select(0));
        files.respond(Hit::Delete);
        files.respond(Hit::Confirm);
        assert!(!fixture.0.join("documents/Alpha.txt").exists());
        files.respond(Hit::NewFolder);
        files.respond(Hit::Key(Key::Char('A')));
        files.respond(Hit::Submit);
        files.respond(Hit::Open(0));
        assert_eq!(files.view().directory, "Documents / A");
        fs::remove_dir(fixture.0.join("documents/A")).unwrap();
        files.storage_returned().unwrap();
        assert_eq!(files.view().directory, "Documents");
        assert_eq!(files.view().rows[0].name, "Beta.txt");
    }

    #[test]
    fn renaming_a_clipboard_source_invalidates_the_previous_path() {
        let fixture = Fixture::new();
        let mut files = Files::new(fixture.0.clone()).unwrap();
        files.respond(Hit::Select(0));
        files.respond(Hit::Move);
        files.respond(Hit::Select(0));
        files.respond(Hit::Rename);
        files.respond(Hit::Key(Key::Char('2')));
        files.respond(Hit::Submit);
        assert!(fixture.0.join("documents/Alpha.txt2").exists());
        assert!(files.view().clipboard.is_none());
    }

    #[test]
    fn reader_actions_follow_file_support_and_refresh_after_usb() {
        use std::os::unix::fs::PermissionsExt;

        let fixture = Fixture::new();
        fs::write(fixture.0.join("documents/Lecture.PDF"), b"pdf").unwrap();
        fs::write(fixture.0.join("documents/Photo.JPEG"), b"image").unwrap();
        fs::write(fixture.0.join("documents/Study.Markdown"), b"notes").unwrap();
        fs::write(fixture.0.join("downloads/Lecture.PDF"), b"pdf").unwrap();
        let mut files = Files::new(fixture.0.clone()).unwrap();
        files.respond(Hit::Select(2));
        let Panel::Actions { readers, .. } = files.view().panel else {
            panic!("expected file actions");
        };
        assert!(!readers.primitive_reader);
        files.respond(Hit::Open(2));
        assert!(files
            .view()
            .notice
            .unwrap()
            .contains("Install Viewer, KOReader, or Reader"));
        assert!(files.pending_open().borrow().is_none());

        let viewer = fixture.0.join("viewer/bin/kl-viewer-app");
        fs::create_dir_all(viewer.parent().unwrap()).unwrap();
        fs::write(&viewer, b"viewer").unwrap();
        fs::set_permissions(&viewer, fs::Permissions::from_mode(0o700)).unwrap();
        let koreader = fixture.0.join("koreader/koreader.sh");
        fs::create_dir(koreader.parent().unwrap()).unwrap();
        fs::write(&koreader, b"reader").unwrap();
        let installed_reader = fixture.0.join("reader/bin/reader-kindle");
        fs::create_dir_all(installed_reader.parent().unwrap()).unwrap();
        fs::write(&installed_reader, b"reader").unwrap();
        fs::set_permissions(&installed_reader, fs::Permissions::from_mode(0o700)).unwrap();
        files.storage_returned().unwrap();

        for (index, viewer, koreader, preferred) in [
            (0, false, true, Reader::KoReader),
            (2, true, true, Reader::Viewer),
            (3, true, false, Reader::Viewer),
            (4, false, false, Reader::PrimitiveReader),
        ] {
            files.respond(Hit::Select(index));
            let Panel::Actions { readers, .. } = files.view().panel else {
                panic!("expected file actions");
            };
            assert_eq!((readers.viewer, readers.koreader), (viewer, koreader));
            assert!(readers.primitive_reader);
            assert_eq!(readers.preferred(), Some(preferred));
        }
        files.respond(Hit::Select(2));
        assert_eq!(
            files.view().hit(Input::Tap(400.0, 760.0)),
            Hit::OpenIn(Reader::PrimitiveReader)
        );
        files.respond(Hit::Cancel);
        files.respond(Hit::Up);
        files.respond(Hit::Open(1));
        files.respond(Hit::Select(0));
        let Panel::Actions { readers, .. } = files.view().panel else {
            panic!("expected download file actions");
        };
        assert!(readers.viewer && readers.koreader);
        assert!(!readers.primitive_reader);
    }
}
