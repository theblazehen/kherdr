//! Retained terminal paint slots and image layers; no connection or input policy.
use crate::{
    appearance::TextSize,
    graphics,
    terminal::{self, CursorStyle},
    ui::{AppWindow, BackgroundRun, CellView, ImageView, RowView},
};
use slint::{Model, ModelRc, VecModel};
use std::rc::Rc;

pub(crate) struct Presentation {
    pub(crate) rows: Rc<VecModel<RowView>>,
    graphics: graphics::GraphicsRenderer,
    pub(crate) paints: u64,
    pub(crate) rows_painted: u64,
}
impl Presentation {
    pub(crate) fn new(rows: Rc<VecModel<RowView>>) -> Self {
        Self {
            rows,
            graphics: graphics::GraphicsRenderer::default(),
            paints: 0,
            rows_painted: 0,
        }
    }
    pub(crate) fn reset_images(&mut self, ui: &AppWindow) {
        self.graphics = graphics::GraphicsRenderer::default();
        ui.set_images_below_background(ModelRc::default());
        ui.set_images_below_text(ModelRc::default());
        ui.set_images_above_text(ModelRc::default());
    }
    pub(crate) fn paint(
        &mut self,
        ui: &AppWindow,
        snapshot: terminal::Snapshot,
        text_size: TextSize,
    ) -> Result<(), String> {
        if self.rows.row_count() != snapshot.rows as usize {
            self.rows.set_vec(
                (0..snapshot.rows)
                    .map(|index| RowView {
                        index: index.into(),
                        ..Default::default()
                    })
                    .collect::<Vec<_>>(),
            );
        }
        let mut painted = 0;
        for row in snapshot.changed_rows {
            let cell_view = |cell: terminal::Cell, previous: Option<&CellView>| {
                let background = color(cell.background);
                // Preserve intentionally invisible cells, but make every visible
                // glyph fully contrast with its background, including dim ANSI ink.
                let foreground = if cell.foreground == cell.background {
                    background
                } else if background.red() == 0 {
                    slint::Color::from_rgb_u8(255, 255, 255)
                } else {
                    slint::Color::from_rgb_u8(0, 0, 0)
                };
                let text = match previous {
                    Some(previous) if previous.text.as_str() == cell.text.as_str() => {
                        previous.text.clone()
                    }
                    _ => cell.text.into(),
                };
                CellView {
                    column: cell.column.into(),
                    span: cell.width.into(),
                    text,
                    foreground,
                    background,
                    background_is_default: cell.background_is_default,
                    bold: cell.bold,
                    italic: cell.italic,
                    underline: cell.underline,
                    strikethrough: cell.strikethrough,
                }
            };
            // Keep complete cells for cursor lookup, selection and copy, but
            // only materialize Slint paint items for ink and explicit backgrounds.
            let mut retained = self
                .rows
                .row_data(row.index as usize)
                .ok_or("Missing paint row")?;
            let cells_replaced = retained.cells.row_count() != row.cells.len();
            let mut changed = cells_replaced;
            if cells_replaced {
                let cells = row
                    .cells
                    .into_iter()
                    .map(|cell| cell_view(cell, None))
                    .collect::<Vec<_>>();
                retained.cells = ModelRc::new(VecModel::from(cells));
            } else {
                for (index, cell) in row.cells.into_iter().enumerate() {
                    let old = retained.cells.row_data(index);
                    let cell = cell_view(cell, old.as_ref());
                    if old.as_ref() != Some(&cell) {
                        retained.cells.set_row_data(index, cell);
                        changed = true;
                    }
                }
            }
            if !changed {
                continue;
            }
            let paint_replaced = update_row_paint(&mut retained);
            if cells_replaced || paint_replaced {
                self.rows.set_row_data(row.index as usize, retained);
            }
            painted += 1;
        }
        self.rows_painted += painted;
        if painted != 0 {
            self.paints += 1;
        }
        let (cell_width, cell_height, _) = text_size.metrics();
        if let Some(layers) = self.graphics.update(
            snapshot.images,
            u32::from(snapshot.cols) * u32::from(cell_width),
            u32::from(snapshot.rows) * u32::from(cell_height),
        )? {
            let model = |sprites: Vec<graphics::Sprite>| {
                ModelRc::new(VecModel::from(
                    sprites
                        .into_iter()
                        .map(|sprite| ImageView {
                            x: sprite.x,
                            y: sprite.y,
                            width: sprite.width as i32,
                            height: sprite.height as i32,
                            source: sprite.image,
                        })
                        .collect::<Vec<_>>(),
                ))
            };
            ui.set_images_below_background(model(layers.below_background));
            ui.set_images_below_text(model(layers.below_text));
            ui.set_images_above_text(model(layers.above_text));
        }
        ui.set_terminal_background(color(snapshot.default_background));
        ui.set_columns(snapshot.cols.into());
        ui.set_cursor_visible(snapshot.cursor.is_some());
        if let Some(cursor) = snapshot.cursor {
            // Retained rows omit wide tails. Find the covering lead without
            // scanning the row or copying its strings into a second cell model.
            let row = self
                .rows
                .row_data(usize::from(cursor.row))
                .ok_or("Missing cursor row")?;
            let column = i32::from(cursor.column);
            let (mut start, mut end) = (0, row.cells.row_count());
            let cell = loop {
                if start == end {
                    return Err("Missing cursor cell".into());
                }
                let middle = start + (end - start) / 2;
                let cell = row.cells.row_data(middle).ok_or("Missing retained cell")?;
                if column < cell.column {
                    end = middle;
                } else if column >= cell.column + cell.span.max(1) {
                    start = middle + 1;
                } else {
                    break cell;
                }
            };
            ui.set_cursor_column(cell.column);
            ui.set_cursor_row(cursor.row.into());
            ui.set_cursor_span(cell.span.max(1));
            ui.set_cursor_cell(cell);
            ui.set_cursor_style(match cursor.style {
                CursorStyle::Block => 0,
                CursorStyle::Bar => 1,
                CursorStyle::Underline => 2,
            });
        }
        Ok(())
    }
}

fn color(rgb: [u8; 3]) -> slint::Color {
    // Text surfaces use solid ink/paper, not subtle luminance differences.
    // Image pixels retain their grayscale in GraphicsRenderer.
    let luminance = u32::from(rgb[0]) * 299 + u32::from(rgb[1]) * 587 + u32::from(rgb[2]) * 114;
    let value = if luminance >= 128_000 { 255 } else { 0 };
    slint::Color::from_rgb_u8(value, value, value)
}

// Retain the row's high-water paint slots: destroying Slint items forces a full
// software-renderer refresh. Default entries paint nothing; capacity is bounded
// by the terminal columns and resets with row geometry.
fn update_paint_model<T: Clone + Default + PartialEq + 'static>(
    model: &mut ModelRc<T>,
    values: Vec<T>,
) -> bool {
    let Some(retained) = model.as_any().downcast_ref::<VecModel<T>>() else {
        *model = ModelRc::new(VecModel::from(values));
        return true;
    };
    let old_len = retained.row_count();
    let new_len = values.len();
    for (index, value) in values.into_iter().enumerate() {
        if index >= old_len {
            retained.push(value);
        } else if retained.row_data(index).as_ref() != Some(&value) {
            retained.set_row_data(index, value);
        }
    }
    for index in new_len..old_len {
        let empty = T::default();
        if retained.row_data(index).as_ref() != Some(&empty) {
            retained.set_row_data(index, empty);
        }
    }
    false
}

fn update_row_paint(row: &mut RowView) -> bool {
    let mut glyphs = Vec::new();
    let mut backgrounds: Vec<BackgroundRun> = Vec::new();
    for cell in row.cells.iter() {
        // Default backgrounds must stay transparent to below-background images.
        // Equal adjacent explicit colors can share a rectangle, including wide
        // leads; a default cell or a gap can never be bridged by that rectangle.
        if !cell.background_is_default {
            let span = cell.span.max(1);
            if let Some(run) = backgrounds.last_mut().filter(|run| {
                run.column + run.span == cell.column && run.background == cell.background
            }) {
                run.span += span;
            } else {
                backgrounds.push(BackgroundRun {
                    column: cell.column,
                    span,
                    background: cell.background,
                });
            }
        }
        // Only plain ASCII spaces/empty text are certainly ink-free. Decorations
        // on blanks still paint. Keep background-colored text too: below-text
        // images can make that ink observable, and cursor/selection use full cells.
        if cell.underline
            || cell.strikethrough
            || cell.text.as_str().bytes().any(|byte| byte != b' ')
        {
            glyphs.push(cell);
        }
    }
    let glyphs_replaced = update_paint_model(&mut row.glyphs, glyphs);
    let backgrounds_replaced = update_paint_model(&mut row.backgrounds, backgrounds);
    glyphs_replaced || backgrounds_replaced
}
