//! Files screen and touch geometry. Filesystem work stays in the controller.

use primitive_ui::{
    back_button, chevron, full_screen_message, header, keyboard_key, screen_rect, Canvas, Editor,
    Icon, Input, Key, Layout, Rect, TextStyle, BLACK, CAPTION, HAIRLINE, PAPER,
};

const WIDTH: i32 = primitive_app::WIDTH as i32;
const HEIGHT: i32 = primitive_app::HEIGHT as i32;
const ROW_HEIGHT: i32 = 128;
const LIST_TOP: i32 = 384;
const CLIPBOARD_HEIGHT: i32 = 120;

const FOLDER: Icon = Icon {
    stroke: &["M2 6 H9 L11 8 H22 V20 H2 Z"],
    fill: &[],
};
const FILE: Icon = Icon {
    stroke: &["M5 2 H14 L19 7 V22 H5 Z M14 2 V7 H19 M8 12 H16 M8 16 H16"],
    fill: &[],
};

pub struct Row {
    pub name: String,
    pub detail: String,
    pub folder: bool,
}

#[derive(Clone, Copy)]
pub enum Panel<'a> {
    None,
    Search { editor: &'a Editor, keyboard: bool },
    Name { title: &'a str, editor: &'a Editor },
    Actions { name: &'a str },
    Delete { name: &'a str },
    Usb,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    None,
    Exit,
    Up,
    Scroll(isize),
    Search,
    NewFolder,
    Paste,
    ClearClipboard,
    Open(usize),
    Select(usize),
    OpenSelected,
    Copy,
    Move,
    Rename,
    Delete,
    Confirm,
    Cancel,
    Submit,
    Key(Key),
}

pub struct View<'a> {
    pub directory: &'a str,
    pub rows: &'a [Row],
    pub first: usize,
    pub can_go_up: bool,
    pub can_edit: bool,
    pub notice: Option<&'a str>,
    pub panel: Panel<'a>,
    /// A short description, such as “Copying report.pdf”.
    pub clipboard: Option<&'a str>,
}

fn rect(x: i32, y: i32, w: i32, h: i32) -> Rect {
    screen_rect(WIDTH, x, y, w, h)
}

fn keyboard_layout() -> Layout {
    Layout::new(WIDTH as u32, HEIGHT as u32, 4, 5)
}

impl View<'_> {
    fn searching(&self) -> bool {
        matches!(self.panel, Panel::Search { .. })
    }

    fn keyboard_visible(&self) -> bool {
        matches!(self.panel, Panel::Search { keyboard: true, .. })
    }

    fn list(&self) -> Rect {
        let top = LIST_TOP
            + if self.clipboard.is_some() {
                CLIPBOARD_HEIGHT
            } else {
                0
            };
        let mut list = rect(40, top, 1184, 0);
        let bottom = if self.keyboard_visible() {
            keyboard_layout().keyboard_area().y - rect(0, 0, 0, 54).h
        } else {
            HEIGHT - rect(0, 0, 0, 130).h
        };
        list.h = (bottom - list.y).max(0);
        list
    }

    pub fn visible_rows(&self) -> usize {
        (self.list().h / rect(0, 0, 0, ROW_HEIGHT).h) as usize
    }

    pub fn max_first(&self) -> usize {
        self.rows.len().saturating_sub(self.visible_rows())
    }

    fn first_visible(&self) -> usize {
        self.first.min(self.max_first())
    }

    /// A visible slot counted from the top of the list, independent of the scroll offset.
    pub fn row_rect(&self, slot: usize) -> Rect {
        let list = self.list();
        let height = rect(0, 0, 0, ROW_HEIGHT).h;
        Rect::new(list.x, list.y + slot as i32 * height, list.w, height)
    }

    fn up_button(&self) -> Rect {
        rect(40, 153, 156, 96)
    }

    fn search_button(&self) -> Rect {
        let width = if self.searching() {
            916
        } else if self.can_edit {
            790
        } else {
            1184
        };
        rect(40, 269, width, 100)
    }

    fn new_folder_button(&self) -> Rect {
        rect(854, 269, 370, 100)
    }

    fn close_search_button(&self) -> Rect {
        rect(980, 269, 244, 100)
    }

    fn paste_button(&self) -> Rect {
        rect(780, LIST_TOP, 214, 92)
    }

    fn clear_clipboard_button(&self) -> Rect {
        rect(1014, LIST_TOP, 210, 92)
    }

    fn row_menu(&self, slot: usize) -> Rect {
        let row = self.row_rect(slot);
        let width = rect(0, 0, 108, 0).w;
        Rect::new(row.right() - width, row.y, width, row.h)
    }

    fn actions_popup(&self) -> Rect {
        rect(166, 436, 932, 560)
    }

    fn actions(&self) -> [(Rect, &'static str, Hit); 6] {
        [
            (rect(214, 598, 408, 100), "Open", Hit::OpenSelected),
            (rect(642, 598, 408, 100), "Rename", Hit::Rename),
            (rect(214, 720, 408, 100), "Copy", Hit::Copy),
            (rect(642, 720, 408, 100), "Move", Hit::Move),
            (rect(214, 842, 408, 100), "Cancel", Hit::Cancel),
            (rect(642, 842, 408, 100), "Delete", Hit::Delete),
        ]
    }

    fn delete_popup(&self) -> Rect {
        rect(166, 540, 932, 520)
    }

    fn delete_buttons(&self) -> [Rect; 2] {
        [rect(214, 900, 408, 100), rect(642, 900, 408, 100)]
    }

    fn save_button(&self) -> Rect {
        rect(896, 400, 328, 100)
    }

    pub fn draw(&self, canvas: &mut Canvas) {
        match self.panel {
            Panel::Usb => full_screen_message(
                canvas,
                "USB drive",
                "Eject your Kindle from the computer to return to Files.",
            ),
            Panel::Name { title, editor } => self.draw_name(canvas, title, editor),
            _ => {
                self.draw_toolbar(canvas);
                self.draw_list(canvas);
                self.draw_notice(canvas);
                match self.panel {
                    Panel::Search {
                        editor,
                        keyboard: true,
                    } => {
                        canvas.keyboard(editor.layer, "Done", None);
                    }
                    Panel::Actions { name } => self.draw_actions(canvas, name),
                    Panel::Delete { name } => self.draw_delete(canvas, name),
                    _ => {}
                }
            }
        }
    }

    fn draw_toolbar(&self, canvas: &mut Canvas) {
        header(canvas, "Files", "Done");
        let path = if self.can_go_up {
            canvas.button(self.up_button(), "Up", false, false);
            rect(225, 173, 999, 70)
        } else {
            rect(40, 173, 1184, 70)
        };
        canvas.text(path, self.directory, TextStyle::new(38).bold());
        if let Panel::Search { editor, keyboard } = self.panel {
            let field = self.search_button();
            if keyboard {
                canvas.single_line_field(field, editor, "Search this folder", false);
            } else {
                let pressed = canvas.control_pressed(field, false);
                canvas.panel(field, 20, 3, if pressed { 220 } else { PAPER });
                let query = editor.string();
                let label = if query.is_empty() {
                    "Search this folder"
                } else {
                    &query
                };
                canvas.text(field.inset(canvas.px(24)), label, TextStyle::new(36));
            }
            canvas.button(self.close_search_button(), "Close", false, false);
        } else {
            canvas.button(self.search_button(), "Search files", false, false);
            if self.can_edit {
                canvas.button(self.new_folder_button(), "New folder", true, false);
            }
        }
        if let Some(clipboard) = self.clipboard {
            let width = if self.can_edit { 710 } else { 950 };
            canvas.text(
                rect(40, LIST_TOP + 29, width, 58),
                clipboard,
                CAPTION.lines(2),
            );
            if self.can_edit {
                canvas.button(self.paste_button(), "Paste", true, false);
            }
            canvas.button(self.clear_clipboard_button(), "Clear", false, false);
        }
    }

    fn draw_list(&self, canvas: &mut Canvas) {
        let first = self.first_visible();
        let rows = &self.rows[first..];
        let count = rows.len().min(self.visible_rows());
        if count == 0 {
            let label = if matches!(self.panel, Panel::Search { editor, .. } if !editor.text.is_empty())
            {
                "No files match your search."
            } else {
                "This folder is empty."
            };
            let list = self.list();
            canvas.text(
                Rect::new(list.x, list.y + canvas.px(48), list.w, canvas.px(80)),
                label,
                CAPTION.center(),
            );
            return;
        }
        let mut card = self.row_rect(0);
        card.h *= count as i32;
        canvas.card(card);
        for (slot, file) in rows.iter().take(count).enumerate() {
            self.draw_row(canvas, slot, file);
            if slot + 1 < count {
                let row = self.row_rect(slot);
                let pad = canvas.px(28);
                canvas.fill(
                    Rect::new(row.x + pad, row.bottom() - 2, row.w - 2 * pad, 2),
                    HAIRLINE,
                );
            }
        }
        let max_first = self.max_first();
        if max_first != 0 {
            let track = self.row_rect(0).h * self.visible_rows() as i32;
            let thumb = (track as usize * count / self.rows.len()) as i32;
            let thumb = thumb.clamp(canvas.px(36), track);
            let offset = (track - thumb) as usize * first / max_first;
            canvas.fill(
                Rect::new(
                    card.right() + canvas.px(12),
                    card.y + offset as i32,
                    canvas.px(5),
                    thumb,
                ),
                120,
            );
        }
    }

    fn draw_row(&self, canvas: &mut Canvas, slot: usize, file: &Row) {
        let row = self.row_rect(slot);
        let menu = self.row_menu(slot);
        let open = if self.can_edit {
            Rect {
                w: row.w - menu.w,
                ..row
            }
        } else {
            row
        };
        let pressed = canvas.control_pressed(open, false);
        if pressed {
            canvas.panel(open.inset(canvas.px(5)), 16, 2, BLACK);
        }
        let ink = if pressed { PAPER } else { BLACK };
        let size = canvas.px(55);
        let icon = Rect::new(
            row.x + canvas.px(29),
            row.y + (row.h - size) / 2,
            size,
            size,
        );
        canvas.icon(icon, if file.folder { FOLDER } else { FILE }, ink);
        let start = row.x + canvas.px(112);
        let width = (open.right() - start - canvas.px(60)).max(1);
        canvas.text(
            Rect::new(start, row.y + canvas.px(20), width, canvas.px(53)),
            &file.name,
            TextStyle::new(37).bold().ink(ink),
        );
        canvas.text(
            Rect::new(start, row.y + canvas.px(72), width, canvas.px(45)),
            &file.detail,
            if pressed { CAPTION.ink(220) } else { CAPTION },
        );
        if file.folder {
            let size = canvas.px(33);
            chevron(
                canvas,
                Rect::new(
                    open.right() - canvas.px(42),
                    row.y + (row.h - size) / 2,
                    size,
                    size,
                ),
                ink,
            );
        }
        if self.can_edit {
            let active = canvas.control_pressed(menu, false);
            let fill = if active { BLACK } else { PAPER };
            let pill_w = canvas.px(68);
            let pill_h = canvas.px(62);
            canvas.pill(
                Rect::new(
                    menu.x + (menu.w - pill_w) / 2,
                    menu.y + (menu.h - pill_h) / 2,
                    pill_w,
                    pill_h,
                ),
                2,
                fill,
            );
            let size = canvas.px(7).max(1);
            for x in [-13, 0, 13] {
                canvas.circle(
                    Rect::new(
                        menu.x + (menu.w - size) / 2 + canvas.px(x),
                        menu.y + (menu.h - size) / 2,
                        size,
                        size,
                    ),
                    if active { PAPER } else { BLACK },
                );
            }
        }
    }

    fn draw_notice(&self, canvas: &mut Canvas) {
        if let Some(notice) = self.notice {
            let bounds = if self.keyboard_visible() {
                let top = keyboard_layout().keyboard_area().y;
                Rect::new(
                    canvas.px(40),
                    top - canvas.px(47),
                    canvas.px(1184),
                    canvas.px(42),
                )
            } else {
                rect(40, 1560, 1184, 100)
            };
            canvas.text(
                bounds,
                notice,
                CAPTION.lines(if self.keyboard_visible() { 1 } else { 2 }),
            );
        }
    }

    fn draw_name(&self, canvas: &mut Canvas, title: &str, editor: &Editor) {
        header(canvas, title, "Cancel");
        canvas.text(rect(40, 193, 1184, 60), "Name", TextStyle::new(36).bold());
        canvas.single_line_field(rect(40, 267, 1184, 104), editor, "Enter a name", false);
        canvas.button(self.save_button(), "Save", true, false);
        if let Some(notice) = self.notice {
            canvas.text(rect(40, 534, 1184, 196), notice, CAPTION.lines(3));
        }
        canvas.keyboard(editor.layer, "Save", None);
    }

    fn draw_actions(&self, canvas: &mut Canvas, name: &str) {
        canvas.popup(self.actions_popup());
        canvas.text(rect(214, 489, 836, 83), name, TextStyle::new(46).bold());
        for (bounds, label, action) in self.actions() {
            canvas.button(bounds, label, action == Hit::OpenSelected, false);
        }
    }

    fn draw_delete(&self, canvas: &mut Canvas, name: &str) {
        canvas.popup(self.delete_popup());
        canvas.text(
            rect(214, 595, 836, 80),
            "Delete this item?",
            TextStyle::new(50).bold(),
        );
        canvas.text(rect(214, 689, 836, 102), name, TextStyle::new(36).lines(2));
        canvas.text(
            rect(214, 800, 836, 60),
            "The item will be permanently removed.",
            CAPTION,
        );
        let [cancel, delete] = self.delete_buttons();
        canvas.button(cancel, "Cancel", false, false);
        canvas.button(delete, "Delete", true, false);
    }

    pub fn hit(&self, input: Input) -> Hit {
        match self.panel {
            Panel::Usb => Hit::None,
            Panel::Name { editor, .. } => self.name_hit(input, editor),
            Panel::Actions { .. } => self.popup_hit(input, self.actions_popup(), &self.actions()),
            Panel::Delete { .. } => {
                let [cancel, delete] = self.delete_buttons();
                self.popup_hit(
                    input,
                    self.delete_popup(),
                    &[
                        (cancel, "Cancel", Hit::Cancel),
                        (delete, "Delete", Hit::Confirm),
                    ],
                )
            }
            _ => self.list_hit(input),
        }
    }

    fn name_hit(&self, input: Input, editor: &Editor) -> Hit {
        match input {
            Input::PageBack => Hit::Cancel,
            Input::Tap(x, y) if back_button(WIDTH).contains(x, y) => Hit::Cancel,
            Input::Tap(x, y) if self.save_button().contains(x, y) => Hit::Submit,
            Input::Tap(x, y) => {
                keyboard_key(&keyboard_layout(), editor, "Save", x, y).map_or(Hit::None, Hit::Key)
            }
            _ => Hit::None,
        }
    }

    fn popup_hit(&self, input: Input, popup: Rect, actions: &[(Rect, &str, Hit)]) -> Hit {
        match input {
            Input::PageBack => Hit::Cancel,
            Input::Tap(x, y) => actions
                .iter()
                .find(|(bounds, _, _)| bounds.contains(x, y))
                .map_or(
                    if popup.contains(x, y) {
                        Hit::None
                    } else {
                        Hit::Cancel
                    },
                    |a| a.2,
                ),
            _ => Hit::None,
        }
    }

    fn list_hit(&self, input: Input) -> Hit {
        let page = self.visible_rows().saturating_sub(1).max(1) as isize;
        let list = self.list();
        match input {
            Input::Swipe { dx, dy, start_y }
                if (list.y as f32..list.bottom() as f32).contains(&start_y)
                    && dy.abs() >= 50.0
                    && dy.abs() > dx.abs() =>
            {
                let distance = (dy.abs() / self.row_rect(0).h as f32).round().max(1.0) as isize;
                Hit::Scroll(if dy < 0.0 { distance } else { -distance })
            }
            Input::PageForward => Hit::Scroll(page),
            Input::PageBack if self.keyboard_visible() => Hit::Key(Key::Go),
            Input::PageBack if self.first_visible() != 0 => Hit::Scroll(-page),
            Input::PageBack if self.searching() => Hit::Cancel,
            Input::PageBack if self.can_go_up => Hit::Up,
            Input::LongTap(x, y) if self.can_edit => self
                .row_at(x, y)
                .map_or(Hit::None, |(_, index)| Hit::Select(index)),
            Input::Tap(x, y) => self.list_tap(x, y),
            _ => Hit::None,
        }
    }

    fn row_at(&self, x: f32, y: f32) -> Option<(usize, usize)> {
        let list = self.list();
        if !list.contains(x, y) {
            return None;
        }
        let slot = ((y - list.y as f32) / self.row_rect(0).h as f32) as usize;
        let index = self.first_visible() + slot;
        (slot < self.visible_rows() && index < self.rows.len()).then_some((slot, index))
    }

    fn list_tap(&self, x: f32, y: f32) -> Hit {
        if back_button(WIDTH).contains(x, y) {
            return Hit::Exit;
        }
        if self.can_go_up && self.up_button().contains(x, y) {
            return Hit::Up;
        }
        if self.search_button().contains(x, y) {
            return Hit::Search;
        }
        if let Panel::Search { editor, keyboard } = self.panel {
            if self.close_search_button().contains(x, y) {
                return Hit::Cancel;
            }
            if keyboard && keyboard_layout().keyboard_area().contains(x, y) {
                return keyboard_key(&keyboard_layout(), editor, "Done", x, y)
                    .map_or(Hit::None, Hit::Key);
            }
        } else if self.can_edit && self.new_folder_button().contains(x, y) {
            return Hit::NewFolder;
        }
        if self.clipboard.is_some() {
            if self.can_edit && self.paste_button().contains(x, y) {
                return Hit::Paste;
            }
            if self.clear_clipboard_button().contains(x, y) {
                return Hit::ClearClipboard;
            }
        }
        match self.row_at(x, y) {
            Some((slot, index)) if self.can_edit && self.row_menu(slot).contains(x, y) => {
                Hit::Select(index)
            }
            Some((_, index)) => Hit::Open(index),
            None => Hit::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tap(rect: Rect) -> Input {
        Input::Tap((rect.x + rect.w / 2) as f32, (rect.y + rect.h / 2) as f32)
    }

    #[test]
    fn scrolled_row_targets_and_fixed_controls_share_layout() {
        let rows: Vec<_> = (0..24)
            .map(|i| Row {
                name: format!("File {i}"),
                detail: "PDF".into(),
                folder: false,
            })
            .collect();
        let view = View {
            directory: "Documents",
            rows: &rows,
            first: 5,
            can_go_up: true,
            can_edit: true,
            notice: None,
            panel: Panel::None,
            clipboard: None,
        };
        assert_eq!(view.hit(tap(view.row_rect(0))), Hit::Open(5));
        assert_eq!(view.hit(tap(view.row_menu(0))), Hit::Select(5));
        assert_eq!(view.hit(tap(view.up_button())), Hit::Up);
        assert_eq!(view.hit(tap(back_button(WIDTH))), Hit::Exit);
        assert_eq!(
            view.hit(Input::Swipe {
                dx: 10.0,
                dy: -260.0,
                start_y: 800.0
            }),
            Hit::Scroll(2),
        );
        assert_eq!(
            view.hit(Input::Swipe {
                dx: 10.0,
                dy: -260.0,
                start_y: 80.0
            }),
            Hit::None,
        );
    }

    #[test]
    fn keyboard_and_confirmation_block_taps_on_the_list_beneath() {
        let rows = [Row {
            name: "Guide.pdf".into(),
            detail: "PDF".into(),
            folder: false,
        }];
        let mut view = View {
            directory: "Documents",
            rows: &rows,
            first: 0,
            can_go_up: true,
            can_edit: true,
            notice: None,
            panel: Panel::None,
            clipboard: None,
        };
        let editor = Editor::new(100);
        let full_rows = view.visible_rows();
        view.panel = Panel::Search {
            editor: &editor,
            keyboard: true,
        };
        assert!(view.visible_rows() < full_rows);
        let go = keyboard_layout()
            .keyboard_keys_with(editor.layer, "Done")
            .into_iter()
            .find(|(key, _)| key.key == Key::Go)
            .unwrap()
            .1;
        assert_eq!(view.hit(tap(go)), Hit::Key(Key::Go));
        view.panel = Panel::Delete { name: "Guide.pdf" };
        assert_eq!(view.hit(tap(view.row_rect(0))), Hit::Cancel);
        assert_eq!(view.hit(tap(view.delete_buttons()[1])), Hit::Confirm);
        view.panel = Panel::Usb;
        assert_eq!(view.hit(tap(back_button(WIDTH))), Hit::None);
    }

    #[test]
    fn virtual_root_allows_browsing_but_hides_edit_targets() {
        let rows = [Row {
            name: "Documents".into(),
            detail: "Folder".into(),
            folder: true,
        }];
        let view = View {
            directory: "On this Kindle",
            rows: &rows,
            first: 0,
            can_go_up: false,
            can_edit: false,
            notice: None,
            panel: Panel::None,
            clipboard: Some("Copying Guide.pdf"),
        };
        assert_eq!(view.hit(tap(view.row_menu(0))), Hit::Open(0));
        assert_eq!(view.hit(tap(view.paste_button())), Hit::None);
        assert_eq!(
            view.hit(tap(view.clear_clipboard_button())),
            Hit::ClearClipboard
        );
    }
}
