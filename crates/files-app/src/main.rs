use std::path::PathBuf;

use files_kindle::{app::Files, reader};

fn main() -> Result<(), String> {
    let storage = PathBuf::from("/mnt/us");
    let app = Files::new(storage.clone()).map_err(|error| error.to_string())?;
    let pending_open = app.pending_open();
    primitive_app::run(app, &storage.join("files"))?;
    let document = pending_open.borrow_mut().take();
    if let Some(path) = document {
        reader::open(&path, &storage).map_err(|error| error.to_string())?;
    }
    Ok(())
}
