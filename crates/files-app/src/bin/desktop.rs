use std::{
    fs, io,
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
    let row = app.view().row_rect(1);
    app.input(Input::LongTap(
        (row.x + 140) as f32,
        (row.y + row.h / 2) as f32,
    ));
    assert!(matches!(
        app.view().panel,
        files_kindle::ui::Panel::Actions { .. }
    ));
    shot(&app, &mut canvas, "files-actions", false)?;
    let browse = Files::new(output.join("sample-kindle"))?;
    shot(&browse, &mut canvas, "files-dark", true)?;
    Ok(())
}
