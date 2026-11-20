//! Give the display to the installed KOReader through the existing Launcher app session.

use std::env;
use std::ffi::{OsStr, OsString};
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

const FRAMEWORK_COMMAND_SHIM: &[u8] = br#"#!/bin/sh
# Launcher already owns restoration of the Kindle interface.
[ "$1" = lab126_gui ] && exit 0
case "$0" in
    */start) exec "$FILES_KOREADER_REAL_START" "$@" ;;
    */stop) exec "$FILES_KOREADER_REAL_STOP" "$@" ;;
esac
exit 127
"#;

pub fn supports(path: &Path) -> bool {
    path.extension().and_then(OsStr::to_str).is_some_and(|ext| {
        [
            "pdf", "epub", "djvu", "djv", "cbz", "fb2", "mobi", "azw3", "txt",
        ]
        .iter()
        .any(|supported| ext.eq_ignore_ascii_case(supported))
    })
}

/// Check before leaving Files so it can show the problem next to the selected document.
pub fn check(path: &Path, root: &Path) -> io::Result<()> {
    primitive_app::with_storage(|| Session::prepare(path, root).map(|_| ()))
}

/// End Files' foreground session by replacing its process with KOReader. Launcher still owns
/// the same process ID and cleans its RAM directory. The storage lease is held across exec.
pub fn open(path: &Path, root: &Path) -> io::Result<()> {
    primitive_app::with_storage(|| {
        let session = Session::prepare(path, root)?;
        let shim = Shim::create(&session.work)?;
        Err(session.command(&shim)?.exec())
    })
}

fn validate_document(path: &Path, root: &Path) -> io::Result<PathBuf> {
    let relative = path.strip_prefix(root).map_err(|_| invalid_document())?;
    if !root.is_absolute()
        || !relative
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
        || !supports(path)
    {
        return Err(invalid_document());
    }
    let absolute = fs::canonicalize(path)?;
    if absolute != fs::canonicalize(root)?.join(relative)
        || !fs::symlink_metadata(&absolute)?.is_file()
    {
        return Err(invalid_document());
    }
    Ok(absolute)
}

fn invalid_document() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "Choose a supported document on this Kindle.",
    )
}

struct Session {
    document: PathBuf,
    reader_script: PathBuf,
    work: PathBuf,
    path: OsString,
    start: PathBuf,
    stop: PathBuf,
}

impl Session {
    fn prepare(document: &Path, root: &Path) -> io::Result<Self> {
        let run = env::var_os("KL_RUN")
            .map(PathBuf::from)
            .unwrap_or_else(|| "/var/tmp/kl".into());
        let app_work = env::var_os("KL_NATIVE_APP_RUN").map(PathBuf::from);
        let supervised = env::var("PRIMITIVE_LAUNCHER").as_deref() == Ok("1")
            && env::var("PRIMITIVE_FRAMEWORK").as_deref() == Ok("stopped");
        if !supervised || !run.is_absolute() || app_work.as_deref() != Some(&run.join("native-app"))
        {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Open Files from Primitive Launcher to use KOReader.",
            ));
        }
        let work = app_work.expect("the supervised app directory matched");
        Self::new(
            document,
            root,
            Path::new("/mnt/us/koreader/koreader.sh"),
            work,
            env::var_os("PATH").unwrap_or_else(|| "/sbin:/bin:/usr/sbin:/usr/bin".into()),
        )
    }

    fn new(
        document: &Path,
        root: &Path,
        reader_script: &Path,
        work: PathBuf,
        path: OsString,
    ) -> io::Result<Self> {
        let document = validate_document(document, root)?;
        if !fs::symlink_metadata(reader_script).is_ok_and(|metadata| metadata.is_file()) {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "Install KOReader to open this document.",
            ));
        }
        if !fs::symlink_metadata(&work)?.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "The Launcher app directory is unavailable.",
            ));
        }
        let start = find_command(&path, "start")?;
        let stop = find_command(&path, "stop")?;
        Ok(Self {
            document,
            reader_script: reader_script.to_owned(),
            work,
            path,
            start,
            stop,
        })
    }

    fn command(&self, shim: &Shim) -> io::Result<Command> {
        let path = env::join_paths(
            std::iter::once(shim.directory.clone()).chain(env::split_paths(&self.path)),
        )
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        // This path is the installed reader's launcher script, never a file selected by the user.
        let reader_home = self
            .reader_script
            .parent()
            .expect("KOReader script has a directory");
        let mut command = Command::new("/bin/sh");
        command
            .current_dir(reader_home)
            .env("KOREADER_DIR", reader_home)
            .env("PATH", path)
            .env("FILES_KOREADER_REAL_START", &self.start)
            .env("FILES_KOREADER_REAL_STOP", &self.stop)
            .arg(&self.reader_script)
            .arg("--framework_stop")
            .arg(&self.document);
        Ok(command)
    }
}

fn find_command(search_path: &OsStr, command: &str) -> io::Result<PathBuf> {
    env::split_paths(search_path)
        .filter(|dir| dir.is_absolute())
        .chain([PathBuf::from("/sbin")])
        .map(|dir| dir.join(command))
        .find(|file| {
            fs::metadata(file).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "The Kindle service controls are unavailable.",
            )
        })
}

struct Shim {
    directory: PathBuf,
}

impl Shim {
    fn create(work: &Path) -> io::Result<Self> {
        let directory = work.join("files-reader");
        DirBuilder::new().mode(0o700).create(&directory)?;
        let shim = Self { directory };
        for command in ["start", "stop"] {
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o700)
                .open(shim.directory.join(command))?
                .write_all(FRAMEWORK_COMMAND_SHIM)?;
        }
        Ok(shim)
    }
}

impl Drop for Shim {
    fn drop(&mut self) {
        // A successful exec leaves cleanup to Launcher. Handle only our own files on error.
        let _ = fs::remove_file(self.directory.join("start"));
        let _ = fs::remove_file(self.directory.join("stop"));
        let _ = fs::remove_dir(&self.directory);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            static NEXT_ID: AtomicU64 = AtomicU64::new(0);
            let dir = env::temp_dir().join(format!(
                "files-reader-{}-{}",
                std::process::id(),
                NEXT_ID.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn confines_reader_documents_to_regular_user_files() {
        let fixture = Fixture::new();
        let root = fixture.0.join("user");
        let docs = root.join("documents");
        fs::create_dir_all(&docs).unwrap();
        let file = docs.join("Lecture 'notes' $2026.PDF");
        fs::write(&file, "example").unwrap();
        assert_eq!(
            validate_document(&file, &root).unwrap(),
            fs::canonicalize(&file).unwrap()
        );
        let alias = root.join("linked");
        symlink(&docs, &alias).unwrap();
        assert!(validate_document(&alias.join(file.file_name().unwrap()), &root).is_err());
        let alias = docs.join("alias.pdf");
        symlink(&file, &alias).unwrap();
        assert!(validate_document(&alias, &root).is_err());
        let outside = fixture.0.join("outside.pdf");
        fs::write(&outside, "example").unwrap();
        assert!(validate_document(&outside, &root).is_err());
        assert!(!supports(&docs.join("Files.sh")));
    }

    #[test]
    fn passes_the_document_as_one_argument_and_preserves_launcher_ownership() {
        let fixture = Fixture::new();
        let root = fixture.0.join("user");
        let work = fixture.0.join("native-app");
        let services = fixture.0.join("existing services");
        for dir in [&root, &work, &services] {
            fs::create_dir(dir).unwrap();
        }
        let reader = root.join("koreader.sh");
        let file = root.join("A 'quoted' \\ file $name.pdf");
        fs::write(&file, "example").unwrap();
        fs::write(&reader, b"printf '%s\\0' \"$@\"\nstart lab126_gui\nstop lab126_gui\nstart ordinary_job\nstop another_job\n").unwrap();
        for verb in ["start", "stop"] {
            let script = format!("#!/bin/sh\nprintf '%s\\0' '{verb}' \"$@\"\n");
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o700)
                .open(services.join(verb))
                .unwrap()
                .write_all(script.as_bytes())
                .unwrap();
        }
        let path = env::join_paths([&services, Path::new("/bin")]).unwrap();
        let session = Session::new(&file, &root, &reader, work.clone(), path).unwrap();
        let shim = Shim::create(&work).unwrap();
        let output = session.command(&shim).unwrap().output().unwrap();
        assert!(output.status.success());
        let args: Vec<_> = output.stdout.split(|&byte| byte == 0).collect();
        assert_eq!(
            args,
            [
                b"--framework_stop",
                session.document.as_os_str().as_encoded_bytes(),
                b"start",
                b"ordinary_job",
                b"stop",
                b"another_job",
                b""
            ]
        );
        drop(shim);
        assert!(!work.join("files-reader").exists());
    }
}
