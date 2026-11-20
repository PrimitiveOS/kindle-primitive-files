use std::{
    fs, io,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use files_kindle::app::Files;
use primitive_app::{font_paths, frame, write_png, App, HEIGHT, WIDTH};
use primitive_ui::{Canvas, Input};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let output = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .unwrap_or_else(|| "build/previews".into()),
    );
    let storage = output.join("sample-kindle");
    let documents = storage.join("documents");
    for folder in ["Books", "Course notes", "Receipts"] {
        fs::create_dir_all(documents.join(folder))?;
    }
    for name in [
        "Atlas.epub",
        "Chemistry reading.pdf",
        "Field journal.txt",
        "Garden plan.pdf",
        "History notes.txt",
        "Journal of walks.pdf",
        "Kindle reading list.txt",
        "Lab handbook.pdf",
        "Meeting agenda.txt",
        "Novel draft.epub",
        "Reference sheet.pdf",
    ] {
        fs::write(documents.join(name), vec![0; 4700 + name.len() * 137])?;
    }
    // Simulate installed readers so the preview can exercise both PDF actions.
    let viewer = storage.join("viewer/bin/kl-viewer-app");
    fs::create_dir_all(storage.join("viewer/bin"))?;
    fs::write(&viewer, b"Desktop preview placeholder only\n")?;
    fs::set_permissions(viewer, fs::Permissions::from_mode(0o700))?;
    fs::create_dir_all(storage.join("koreader"))?;
    fs::write(
        storage.join("koreader/koreader.sh"),
        b"# Desktop preview placeholder only\n",
    )?;
    let fonts = std::env::var_os("PRIMITIVE_FONTS_DIR").map_or_else(
        || {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../doannt-kindle-launcher/assets/fonts")
        },
        PathBuf::from,
    );
    let mut canvas = Canvas::new(WIDTH, HEIGHT, &font_paths(&fonts)).map_err(io::Error::other)?;
    let mut app = Files::new(storage)?;
    let shot = |app: &Files, canvas: &mut Canvas, name: &str, dark| {
        write_png(
            &output.join(format!("{name}.png")),
            WIDTH,
            HEIGHT,
            &frame(app, canvas, dark),
        )
    };
    shot(&app, &mut canvas, "files-browse", false)?;
    app.input(Input::Swipe {
        dx: 0.0,
        dy: -460.0,
        start_y: 1050.0,
    });
    assert!(app.view().first > 0);
    shot(&app, &mut canvas, "files-scrolled", false)?;
    let view = app.view();
    let pdf_slot = view
        .rows
        .iter()
        .skip(view.first)
        .take(view.visible_rows())
        .position(|row| row.name.ends_with(".pdf"))
        .ok_or_else(|| io::Error::other("No visible sample PDF in Files preview"))?;
    let row = view.row_rect(pdf_slot);
    app.input(Input::LongTap(
        (row.x + 140) as f32,
        (row.y + row.h / 2) as f32,
    ));
    let files_kindle::ui::Panel::Actions { readers, .. } = app.view().panel else {
        return Err(io::Error::other("Expected PDF actions in Files preview").into());
    };
    assert!(readers.viewer && readers.koreader);
    shot(&app, &mut canvas, "files-actions", false)?;
    let browse = Files::new(output.join("sample-kindle"))?;
    shot(&browse, &mut canvas, "files-dark", true)?;
    Ok(())
}
