//! Give the display to an installed reader through the existing Launcher app session.

use std::env;
use std::ffi::{OsStr, OsString};
use std::fs::{self, DirBuilder, File, OpenOptions};
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
const VIEWER_BINARY: &str = "viewer/bin/kl-viewer-app";
const KOREADER_SCRIPT: &str = "koreader/koreader.sh";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reader {
    Viewer,
    KoReader,
}

impl Reader {
    pub fn supports(self, path: &Path) -> bool {
        let formats: &[&str] = match self {
            Self::Viewer => &["pdf", "png", "jpg", "jpeg"],
            Self::KoReader => &[
                "pdf", "epub", "djvu", "djv", "cbz", "fb2", "mobi", "azw3", "txt",
            ],
        };
        path.extension().and_then(OsStr::to_str).is_some_and(|ext| {
            formats
                .iter()
                .any(|supported| ext.eq_ignore_ascii_case(supported))
        })
    }

    fn name(self) -> &'static str {
        match self {
            Self::Viewer => "Viewer",
            Self::KoReader => "KOReader",
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Readers {
    pub viewer: bool,
    pub koreader: bool,
}

impl Readers {
    pub fn installed(root: &Path) -> Self {
        Self {
            viewer: viewer_binary(root).is_ok(),
            koreader: fs::symlink_metadata(root.join(KOREADER_SCRIPT))
                .is_ok_and(|metadata| metadata.is_file()),
        }
    }

    pub fn for_path(self, path: &Path) -> Self {
        Self {
            viewer: self.viewer && Reader::Viewer.supports(path),
            koreader: self.koreader && Reader::KoReader.supports(path),
        }
    }

    pub fn preferred(self) -> Option<Reader> {
        match (self.viewer, self.koreader) {
            (true, _) => Some(Reader::Viewer),
            (false, true) => Some(Reader::KoReader),
            (false, false) => None,
        }
    }
}

pub struct OpenRequest {
    pub reader: Reader,
    pub path: PathBuf,
}

/// Check before leaving Files so it can show the problem next to the selected document.
pub fn check(reader: Reader, path: &Path, root: &Path) -> io::Result<()> {
    primitive_app::with_storage(|| match reader {
        Reader::Viewer => ViewerSession::prepare(path, root).map(|_| ()),
        Reader::KoReader => Session::prepare(path, root).map(|_| ()),
    })
}

/// End Files' foreground session by replacing its process with the reader. Launcher still owns
/// the same process ID and cleans its RAM directory. The storage lease is held across exec.
pub fn open(request: &OpenRequest, root: &Path) -> io::Result<()> {
    primitive_app::with_storage(|| match request.reader {
        Reader::Viewer => {
            let session = ViewerSession::prepare(&request.path, root)?;
            let program = ViewerProgram::stage(&session.binary, &session.work)?;
            Err(session.command(&program).exec())
        }
        Reader::KoReader => {
            let session = Session::prepare(&request.path, root)?;
            let shim = Shim::create(&session.work)?;
            Err(session.command(&shim)?.exec())
        }
    })
}

fn validate_document(path: &Path, root: &Path, reader: Reader) -> io::Result<PathBuf> {
    let relative = path.strip_prefix(root).map_err(|_| invalid_document())?;
    if !root.is_absolute()
        || !relative
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
        || !reader.supports(path)
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

fn supervised_work(reader: Reader) -> io::Result<PathBuf> {
    let run = env::var_os("KL_RUN").map_or_else(|| "/var/tmp/kl".into(), PathBuf::from);
    let work = run.join("native-app");
    let managed = env::var("PRIMITIVE_LAUNCHER").as_deref() == Ok("1")
        && env::var("PRIMITIVE_FRAMEWORK").as_deref() == Ok("stopped")
        && run.is_absolute()
        && env::var_os("KL_NATIVE_APP_RUN").as_deref() == Some(work.as_os_str());
    if !managed {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "Open Files from Primitive Launcher to use {}.",
                reader.name()
            ),
        ));
    }
    if !fs::symlink_metadata(&work)?.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "The Launcher app directory is unavailable.",
        ));
    }
    Ok(work)
}

fn viewer_binary(root: &Path) -> io::Result<PathBuf> {
    let expected = fs::canonicalize(root)?.join(VIEWER_BINARY);
    let unavailable =
        || io::Error::new(io::ErrorKind::NotFound, "Install Viewer to open this file.");
    let installed = fs::canonicalize(root.join(VIEWER_BINARY)).map_err(|_| unavailable())?;
    let metadata = fs::symlink_metadata(&installed).map_err(|_| unavailable())?;
    if installed != expected || !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
        return Err(unavailable());
    }
    Ok(installed)
}

struct ViewerSession {
    document: PathBuf,
    binary: PathBuf,
    work: PathBuf,
}

impl ViewerSession {
    fn prepare(document: &Path, root: &Path) -> io::Result<Self> {
        Ok(Self {
            document: validate_document(document, root, Reader::Viewer)?,
            binary: viewer_binary(root)?,
            work: supervised_work(Reader::Viewer)?,
        })
    }

    fn command(&self, program: &ViewerProgram) -> Command {
        let mut command = Command::new(&program.0);
        command.current_dir("/").arg("--open").arg(&self.document);
        command
    }
}

struct ViewerProgram(PathBuf);

impl ViewerProgram {
    fn stage(binary: &Path, work: &Path) -> io::Result<Self> {
        let mut source = File::open(binary)?;
        let path = work.join("files-viewer");
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o700)
            .open(&path)?;
        let program = Self(path);
        io::copy(&mut source, &mut output)?;
        Ok(program)
    }
}

impl Drop for ViewerProgram {
    fn drop(&mut self) {
        // Launcher removes the RAM directory after a successful exec.
        let _ = fs::remove_file(&self.0);
    }
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
        Self::new(
            document,
            root,
            &root.join(KOREADER_SCRIPT),
            supervised_work(Reader::KoReader)?,
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
        let document = validate_document(document, root, Reader::KoReader)?;
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
            validate_document(&file, &root, Reader::KoReader).unwrap(),
            fs::canonicalize(&file).unwrap()
        );
        let alias = root.join("linked");
        symlink(&docs, &alias).unwrap();
        assert!(validate_document(
            &alias.join(file.file_name().unwrap()),
            &root,
            Reader::Viewer
        )
        .is_err());
        let alias = docs.join("alias.pdf");
        symlink(&file, &alias).unwrap();
        assert!(validate_document(&alias, &root, Reader::Viewer).is_err());
        let outside = fixture.0.join("outside.pdf");
        fs::write(&outside, "example").unwrap();
        assert!(validate_document(&outside, &root, Reader::KoReader).is_err());
        assert!(!Reader::KoReader.supports(&docs.join("Files.sh")));
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

    #[test]
    fn stages_the_installed_viewer_and_passes_one_literal_document_argument() {
        let fixture = Fixture::new();
        let root = fixture.0.join("user");
        let work = fixture.0.join("native-app");
        let binary = root.join(VIEWER_BINARY);
        fs::create_dir_all(binary.parent().unwrap()).unwrap();
        fs::create_dir(&work).unwrap();
        let image = root.join("A 'quoted' \\ portrait $name.JPEG");
        fs::write(&image, b"example").unwrap();
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o700)
            .open(&binary)
            .unwrap()
            .write_all(b"#!/bin/sh\nprintf '%s\\0' \"$@\"\n")
            .unwrap();
        let expected = validate_document(&image, &root, Reader::Viewer).unwrap();
        let session = ViewerSession {
            document: expected.clone(),
            binary: viewer_binary(&root).unwrap(),
            work,
        };
        let program = ViewerProgram::stage(&session.binary, &session.work).unwrap();
        let output = session.command(&program).output().unwrap();
        assert!(output.status.success());
        assert_eq!(
            output.stdout.split(|&b| b == 0).collect::<Vec<_>>(),
            [b"--open", expected.as_os_str().as_encoded_bytes(), b""]
        );
        assert!(ViewerProgram::stage(&session.binary, &session.work).is_err());
        drop(program);
        assert!(!session.work.join("files-viewer").exists());

        fs::rename(&binary, binary.with_extension("saved")).unwrap();
        symlink(binary.with_extension("saved"), &binary).unwrap();
        assert!(viewer_binary(&root).is_err());
    }
}
