use std::{
    ffi::OsStr,
    fs::{self, File, Metadata, OpenOptions},
    io,
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    Directory,
    File,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileEntry {
    pub path: PathBuf,
    pub name: String,
    pub kind: EntryKind,
    pub size: u64,
}

pub struct FileStore {
    root: PathBuf,
}

impl FileStore {
    /// The runtime supplies the Kindle user-storage mount and holds a supervisor
    /// storage lease for each mutation. The store retains no open file handles.
    pub fn new(root: impl Into<PathBuf>) -> io::Result<Self> {
        let store = Self { root: root.into() };
        store.checked_dir(Path::new(""))?;
        Ok(store)
    }

    pub fn initial_dir(&self) -> PathBuf {
        self.list(Path::new(""))
            .ok()
            .and_then(|entries| {
                entries
                    .into_iter()
                    .find(|entry| entry.name.eq_ignore_ascii_case("documents"))
            })
            .map(|entry| entry.path)
            .unwrap_or_default()
    }

    /// The empty path represents the storage screen, with shortcuts to approved user folders.
    pub fn list(&self, dir: &Path) -> io::Result<Vec<FileEntry>> {
        let absolute = self.checked_dir(dir)?;
        let mut entries = Vec::new();
        for entry in fs::read_dir(absolute)? {
            let entry = entry?;
            let name = entry.file_name();
            if name.to_string_lossy().starts_with('.') {
                continue;
            }
            let path = dir.join(&name);
            if !allowed_path(&path) {
                continue;
            }
            let file_type = entry.file_type()?;
            let (kind, size) = if file_type.is_dir() {
                (EntryKind::Directory, 0)
            } else if file_type.is_file() && !dir.as_os_str().is_empty() {
                let metadata = fs::symlink_metadata(entry.path())?;
                if !metadata.is_file() {
                    continue;
                }
                (EntryKind::File, metadata.len())
            } else {
                continue;
            };
            entries.push(FileEntry {
                path,
                name: name.to_string_lossy().into_owned(),
                kind,
                size,
            });
        }
        entries.sort_by_cached_key(|entry| {
            (
                entry.kind == EntryKind::File,
                entry.name.to_lowercase(),
                entry.path.clone(),
            )
        });
        Ok(entries)
    }

    pub fn create_dir(&self, dir: &Path, name: &str) -> io::Result<PathBuf> {
        validate_name(name)?;
        let destination = dir.join(name);
        require_mutable(&destination)?;
        fs::create_dir(self.checked_dir(dir)?.join(name)).map_err(destination_error)?;
        Ok(destination)
    }

    pub fn rename(&self, path: &Path, name: &str) -> io::Result<PathBuf> {
        validate_name(name)?;
        let dir = path.parent().ok_or_else(protected)?;
        self.rename_to(path, dir, OsStr::new(name))
    }

    pub fn copy(&self, path: &Path, target_dir: &Path) -> io::Result<PathBuf> {
        let name = path.file_name().ok_or_else(protected)?;
        let (source, metadata, target, relative) = self.transfer_paths(path, target_dir, name)?;
        require_absent(&target)?;
        let stage = CopyStage::new(target.parent().ok_or_else(protected)?)?;
        let copied = stage.0.join("item");
        copy_entry(&source, &copied, &metadata)?;
        require_absent(&target)?;
        fs::rename(&copied, &target).map_err(destination_error)?;
        Ok(relative)
    }

    pub fn move_entry(&self, path: &Path, target_dir: &Path) -> io::Result<PathBuf> {
        let name = path.file_name().ok_or_else(protected)?;
        self.rename_to(path, target_dir, name)
    }

    /// Delete a regular file or an empty folder. Each delete is a single filesystem action.
    pub fn delete(&self, path: &Path) -> io::Result<()> {
        require_mutable(path)?;
        let (absolute, metadata) = self.checked(path)?;
        if metadata.is_file() {
            return fs::remove_file(absolute);
        }
        fs::remove_dir(absolute).map_err(|error| {
            if error.kind() == io::ErrorKind::DirectoryNotEmpty {
                io::Error::new(
                    error.kind(),
                    "Empty the folder, including hidden items, before deleting it.",
                )
            } else {
                error
            }
        })
    }

    pub fn openable_path(&self, path: &Path) -> io::Result<PathBuf> {
        let (absolute, metadata) = self.checked(path)?;
        if metadata.is_file() {
            Ok(absolute)
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Choose a file to open.",
            ))
        }
    }

    fn rename_to(&self, path: &Path, target_dir: &Path, name: &OsStr) -> io::Result<PathBuf> {
        let (source, _, target, relative) = self.transfer_paths(path, target_dir, name)?;
        if source != target {
            // Files serializes operations, and the runtime holds the USB lease for
            // the entire operation. The check and same-volume rename run together.
            require_absent(&target)?;
            fs::rename(&source, &target).map_err(destination_error)?;
        }
        Ok(relative)
    }

    fn transfer_paths(
        &self,
        path: &Path,
        target_dir: &Path,
        name: &OsStr,
    ) -> io::Result<(PathBuf, Metadata, PathBuf, PathBuf)> {
        require_mutable(path)?;
        let relative = target_dir.join(name);
        require_mutable(&relative)?;
        let (source, metadata) = self.checked(path)?;
        let destination_dir = self.checked_dir(target_dir)?;
        if metadata.is_dir() && is_within(&destination_dir, &source) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Choose a destination outside this folder.",
            ));
        }
        Ok((source, metadata, destination_dir.join(name), relative))
    }

    fn checked_dir(&self, path: &Path) -> io::Result<PathBuf> {
        let (absolute, metadata) = self.checked(path)?;
        if metadata.is_dir() {
            Ok(absolute)
        } else {
            Err(io::Error::new(
                io::ErrorKind::NotADirectory,
                "This folder is unavailable.",
            ))
        }
    }

    fn checked(&self, relative: &Path) -> io::Result<(PathBuf, Metadata)> {
        if !allowed_path(relative) {
            return Err(protected());
        }
        let mut absolute = self.root.clone();
        let mut metadata = fs::symlink_metadata(&absolute)?;
        if !metadata.is_dir() {
            return Err(protected());
        }
        for (depth, component) in relative.components().enumerate() {
            if !metadata.is_dir() {
                return Err(io::Error::new(
                    io::ErrorKind::NotADirectory,
                    "This folder is unavailable.",
                ));
            }
            absolute.push(component.as_os_str());
            metadata = fs::symlink_metadata(&absolute)?;
            if depth == 0 && !metadata.is_dir() {
                return Err(io::Error::new(
                    io::ErrorKind::NotADirectory,
                    "This folder is unavailable.",
                ));
            }
            if !metadata.is_dir() && !metadata.is_file() {
                return Err(protected());
            }
        }
        Ok((absolute, metadata))
    }
}

fn allowed_path(relative: &Path) -> bool {
    let mut parts = relative.components();
    let Some(first) = parts.next() else {
        return true;
    };
    let Component::Normal(area) = first else {
        return false;
    };
    let area = area.to_string_lossy();
    if ![
        "documents",
        "download",
        "downloads",
        "pictures",
        "music",
        "audible",
    ]
    .iter()
    .any(|name| area.eq_ignore_ascii_case(name))
    {
        return false;
    }
    let first_inside_area = parts.next();
    if let Some(component) = first_inside_area {
        if !matches!(component, Component::Normal(_)) {
            return false;
        }
        if area.eq_ignore_ascii_case("documents")
            && Path::new(component.as_os_str())
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("sh"))
        {
            return false;
        }
    }
    parts.all(|part| matches!(part, Component::Normal(_)))
}

fn require_mutable(path: &Path) -> io::Result<()> {
    if path.components().count() < 2 || !allowed_path(path) {
        return Err(protected());
    }
    Ok(())
}

fn protected() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "This item is outside the files you can manage.",
    )
}

fn validate_name(name: &str) -> io::Result<()> {
    if name.is_empty()
        || name.starts_with('.')
        || name.trim() != name
        || name.ends_with('.')
        || name.encode_utf16().count() > 255
        || name
            .chars()
            .any(|c| c.is_control() || "\\/:\"*?<>|".contains(c))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Enter a name without slashes, reserved characters, or a leading dot.",
        ));
    }
    Ok(())
}

/// Kindle storage uses case-insensitive names. Treat differently capitalized
/// paths as the same folder when detecting a recursive copy or move.
fn is_within(path: &Path, folder: &Path) -> bool {
    let mut parts = path.components();
    folder.components().all(|part| {
        parts
            .next()
            .is_some_and(|path_part| path_part.as_os_str().eq_ignore_ascii_case(part.as_os_str()))
    })
}

fn require_absent(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => Err(already_exists()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn destination_error(error: io::Error) -> io::Error {
    if error.kind() == io::ErrorKind::AlreadyExists {
        already_exists()
    } else {
        error
    }
}

fn already_exists() -> io::Error {
    io::Error::new(
        io::ErrorKind::AlreadyExists,
        "An item with this name already exists here.",
    )
}

/// Copy into a hidden sibling folder so an interrupted copy never appears at the chosen path.
struct CopyStage(PathBuf);

impl CopyStage {
    fn new(parent: &Path) -> io::Result<Self> {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        for _ in 0..32 {
            let id = NEXT.fetch_add(1, Ordering::Relaxed);
            let directory = parent.join(format!(".primitive-files-{}-{id}", std::process::id()));
            match fs::create_dir(&directory) {
                Ok(()) => return Ok(Self(directory)),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::other(
            "Could not prepare a temporary folder for the copy.",
        ))
    }
}

impl Drop for CopyStage {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A directory copy preserves its complete tree, including hidden files.
fn copy_entry(source: &Path, destination: &Path, metadata: &Metadata) -> io::Result<()> {
    if metadata.is_dir() {
        fs::create_dir(destination)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            let path = entry.path();
            copy_entry(
                &path,
                &destination.join(entry.file_name()),
                &fs::symlink_metadata(&path)?,
            )?;
        }
        return Ok(());
    }
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "This folder contains a link or special file that Files cannot copy.",
        ));
    }
    let mut input = File::open(source)?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    io::copy(&mut input, &mut output)?;
    output.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        directory: PathBuf,
        store: FileStore,
    }

    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let id = NEXT.fetch_add(1, Ordering::Relaxed);
            let directory = std::env::temp_dir()
                .join(format!("primitive-files-test-{}-{id}", std::process::id()));
            fs::create_dir(&directory).unwrap();
            for folder in ["documents", "Download", "launcher"] {
                fs::create_dir(directory.join(folder)).unwrap();
            }
            let store = FileStore::new(&directory).unwrap();
            Self { directory, store }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.directory).unwrap();
        }
    }

    #[test]
    fn browses_only_visible_user_areas_and_sorts_folders_first() {
        let fixture = Fixture::new();
        fs::create_dir(fixture.directory.join("documents/Folder")).unwrap();
        fs::write(fixture.directory.join("documents/a.pdf"), b"PDF").unwrap();
        fs::write(fixture.directory.join("documents/Z.txt"), b"Text").unwrap();
        fs::write(fixture.directory.join("documents/Launcher.sh"), b"launcher").unwrap();
        fs::write(fixture.directory.join("documents/.DS_Store"), b"data").unwrap();
        let shortcuts = fixture.store.list(Path::new("")).unwrap();
        assert_eq!(
            shortcuts
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            ["documents", "Download"]
        );
        assert_eq!(fixture.store.initial_dir(), Path::new("documents"));
        let entries = fixture.store.list(Path::new("documents")).unwrap();
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            ["Folder", "a.pdf", "Z.txt"]
        );
        assert_eq!(
            (entries[0].kind, entries[1].size),
            (EntryKind::Directory, 3)
        );
        assert_eq!(
            fixture.store.openable_path(&entries[1].path).unwrap(),
            fixture.directory.join("documents/a.pdf")
        );
    }

    #[test]
    fn mutates_user_items_without_overwriting_and_refuses_a_nonempty_delete() {
        let fixture = Fixture::new();
        let documents = Path::new("documents");
        let downloads = Path::new("Download");
        let folder = fixture.store.create_dir(documents, "Reports").unwrap();
        fs::write(
            fixture.directory.join(&folder).join("Report.pdf"),
            b"sample",
        )
        .unwrap();
        let dest = fixture.store.copy(&folder, downloads).unwrap();
        assert_eq!(
            fs::read(fixture.directory.join(&dest).join("Report.pdf")).unwrap(),
            b"sample"
        );
        assert_eq!(
            fixture.store.copy(&folder, downloads).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(
            fixture
                .store
                .move_entry(&folder, downloads)
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(
            fixture.store.delete(&folder).unwrap_err().kind(),
            io::ErrorKind::DirectoryNotEmpty
        );
        let original_file = folder.join("Report.pdf");
        let renamed = fixture.store.rename(&original_file, "Draft.pdf").unwrap();
        assert!(fixture.store.openable_path(&original_file).is_err());
        let moved = fixture.store.move_entry(&renamed, downloads).unwrap();
        assert_eq!(fs::read(fixture.directory.join(&moved)).unwrap(), b"sample");
        fixture.store.delete(&moved).unwrap();
        fixture.store.delete(&folder).unwrap();
        assert!(fixture.store.list(&folder).is_err());
    }

    #[test]
    fn rejects_out_of_scope_paths_names_and_recursive_transfers() {
        let fixture = Fixture::new();
        let documents = Path::new("documents");
        let parent = fixture.store.create_dir(documents, "Folder").unwrap();
        let child = fixture.store.create_dir(&parent, "Subfolder").unwrap();
        fs::write(fixture.directory.join("documents/Store.SH"), b"Store").unwrap();
        for blocked in [
            "/etc",
            "launcher",
            "documents/../launcher",
            "documents/Store.SH",
        ] {
            assert!(
                fixture.store.openable_path(Path::new(blocked)).is_err(),
                "{blocked}"
            );
        }
        assert!(fixture.store.delete(documents).is_err());
        assert!(fixture
            .store
            .create_dir(Path::new(""), "documents")
            .is_err());
        for name in ["", ".", "new/name", "new\\name", "one?", "tail."] {
            assert!(fixture.store.create_dir(documents, name).is_err(), "{name}");
        }
        assert!(fixture.store.rename(&parent, "Browser.sh").is_err());
        assert!(fixture.store.copy(&parent, &child).is_err());
        assert!(fixture.store.move_entry(&parent, &child).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn ignores_symlinks_and_rolls_back_a_folder_copy_when_an_entry_is_unsafe() {
        use std::os::unix::fs::symlink;

        let fixture = Fixture::new();
        let folder = fixture
            .store
            .create_dir(Path::new("documents"), "Reports")
            .unwrap();
        fs::write(fixture.directory.join(&folder).join("Regular.pdf"), b"PDF").unwrap();
        symlink(
            fixture.directory.join("launcher"),
            fixture.directory.join(&folder).join("Link"),
        )
        .unwrap();
        assert_eq!(fixture.store.list(&folder).unwrap().len(), 1);
        assert!(fixture.store.list(&folder.join("Link")).is_err());
        assert!(fixture.store.openable_path(&folder.join("Link")).is_err());
        assert!(fixture.store.copy(&folder, Path::new("Download")).is_err());
        assert_eq!(
            fs::read_dir(fixture.directory.join("Download"))
                .unwrap()
                .count(),
            0
        );
    }
}
