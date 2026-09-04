// SPDX-License-Identifier: MIT
// SPDX-FileCopyrightText: 2026 Huang Rui <vowstar@gmail.com>

//! Right properties panel for editing selected element attributes.

use log::{error, info};
use std::io::Cursor;
use std::path::{Path, PathBuf};

use ptouch_render::document::{FontSizeUnit, LayoutMode, QrErrorCorrection};
use ptouch_render::image_loader;
use ptouch_render::text::TextAlign;

use crate::state::{AppState, LabelElement};

/// Render the right-side properties panel.
pub fn show_properties(ui: &mut egui::Ui, state: &mut AppState) {
    ui.heading("Properties");
    ui.add_space(4.0);

    let selected = match state.selected_element {
        Some(idx) if idx < state.elements.len() => idx,
        _ => {
            ui.label("Select an element to edit properties");
            return;
        }
    };

    // We need to clone the element to avoid borrow issues, then write back.
    let mut element = state.elements[selected].clone();
    let mut changed = false;

    match &mut element {
        LabelElement::Text {
            content,
            x,
            y,
            font_name,
            font_weight,
            font_size,
            font_size_unit,
            align,
            rotation,
            flip_h,
            flip_v,
            ..
        } => {
            changed |= show_text_properties(
                ui,
                TextProps {
                    content,
                    x,
                    y,
                    font_name,
                    font_weight,
                    font_size,
                    font_size_unit,
                    align,
                    rotation,
                    flip_h,
                    flip_v,
                },
                state,
            );
        }
        LabelElement::Image {
            path,
            image_data,
            bitmap,
            x,
            y,
            rotation,
            target_height,
            target_width,
            flip_h,
            flip_v,
            ..
        } => {
            changed |= show_image_properties(
                ui,
                ImageProps {
                    path,
                    image_data,
                    bitmap,
                    x,
                    y,
                    rotation,
                    target_height,
                    target_width,
                    flip_h,
                    flip_v,
                },
                state,
            );
        }
        LabelElement::QrCode {
            content,
            x,
            y,
            size,
            error_correction,
            source,
            min_module_size,
        } => {
            changed |= show_qr_properties(
                ui,
                QrProps {
                    content,
                    x,
                    y,
                    size,
                    error_correction,
                    source,
                    min_module_size,
                },
                state,
            );
        }
        LabelElement::CutMark => {
            ui.label("Cut Mark");
            ui.add_space(4.0);
            ui.label("No editable properties.");
        }
        LabelElement::Padding { pixels } => {
            changed |= show_padding_properties(ui, pixels);
        }
    }

    if changed {
        state.elements[selected] = element;
        state.mark_dirty();
    }

    if let Some(bounds) = state.element_bounds.get(selected) {
        ui.separator();
        ui.label(format!(
            "Rendered bounds: x={} y={} {}×{} px",
            bounds.x, bounds.y, bounds.width, bounds.height
        ));
    }
}

/// Show properties for a semantic QR-code element. Returns true if changed.
fn show_qr_properties(ui: &mut egui::Ui, props: QrProps, state: &AppState) -> bool {
    let QrProps {
        content,
        x,
        y,
        size,
        error_correction,
        source,
        min_module_size,
    } = props;
    let mut changed = false;
    ui.label("QR Content:");
    changed |= ui
        .add_enabled(
            source.is_none(),
            egui::TextEdit::multiline(content)
                .desired_width(f32::INFINITY)
                .desired_rows(3),
        )
        .changed();

    if state.layout == LayoutMode::Positioned {
        ui.add_space(4.0);
        changed |= show_position(ui, x, y);
    }

    ui.horizontal(|ui| {
        ui.label("Size:");
        changed |= ui
            .add(egui::DragValue::new(size).speed(1).range(1..=100_000))
            .changed();
        ui.label("px");
    });

    ui.horizontal(|ui| {
        ui.label("Minimum module:");
        changed |= ui
            .add(
                egui::DragValue::new(min_module_size)
                    .speed(1)
                    .range(1..=100),
            )
            .changed();
        ui.label("px");
    });

    ui.add_enabled_ui(source.is_none(), |ui| {
        egui::ComboBox::from_label("Error correction")
            .selected_text(error_correction.as_str().to_uppercase())
            .show_ui(ui, |ui| {
                changed |= ui
                    .selectable_value(error_correction, QrErrorCorrection::Low, "L")
                    .changed();
                changed |= ui
                    .selectable_value(error_correction, QrErrorCorrection::Medium, "M")
                    .changed();
                changed |= ui
                    .selectable_value(error_correction, QrErrorCorrection::Quartile, "Q")
                    .changed();
                changed |= ui
                    .selectable_value(error_correction, QrErrorCorrection::High, "H")
                    .changed();
            });
    });
    if source.is_some() {
        ui.add_space(4.0);
        ui.label("This QR preserves a decoded source symbol's codewords, version, and mask.");
        if ui.button("Use content encoding instead").clicked() {
            *source = None;
            changed = true;
        }
    }
    changed
}

/// Mutable references to a QR-code element's editable fields.
struct QrProps<'a> {
    content: &'a mut String,
    x: &'a mut Option<u32>,
    y: &'a mut Option<u32>,
    size: &'a mut u32,
    error_correction: &'a mut QrErrorCorrection,
    source: &'a mut Option<ptouch_render::document::QrSource>,
    min_module_size: &'a mut u32,
}

/// Mutable references to a text element's editable fields.
struct TextProps<'a> {
    content: &'a mut String,
    x: &'a mut Option<u32>,
    y: &'a mut Option<u32>,
    font_name: &'a mut Option<String>,
    font_weight: &'a mut Option<u16>,
    font_size: &'a mut Option<f32>,
    font_size_unit: &'a mut Option<FontSizeUnit>,
    align: &'a mut TextAlign,
    rotation: &'a mut f32,
    flip_h: &'a mut bool,
    flip_v: &'a mut bool,
}

/// Mutable references to an image element's editable fields.
struct ImageProps<'a> {
    path: &'a mut Option<PathBuf>,
    image_data: &'a mut Vec<u8>,
    bitmap: &'a mut Option<ptouch_render::bitmap::LabelBitmap>,
    x: &'a mut Option<u32>,
    y: &'a mut Option<u32>,
    rotation: &'a mut f32,
    target_height: &'a mut Option<u32>,
    target_width: &'a mut Option<u32>,
    flip_h: &'a mut bool,
    flip_v: &'a mut bool,
}

/// Two checkboxes for per-element horizontal/vertical mirroring. Returns true if
/// either changed. Shared by the text and image property panels.
fn show_flip_controls(ui: &mut egui::Ui, flip_h: &mut bool, flip_v: &mut bool) -> bool {
    let mut changed = false;
    ui.label("Mirror:");
    if ui
        .checkbox(flip_h, "Flip horizontal (left-right)")
        .changed()
    {
        changed = true;
    }
    if ui.checkbox(flip_v, "Flip vertical (top-bottom)").changed() {
        changed = true;
    }
    changed
}

fn show_position(ui: &mut egui::Ui, x: &mut Option<u32>, y: &mut Option<u32>) -> bool {
    let mut changed = x.is_none() || y.is_none();
    let x = x.get_or_insert(0);
    let y = y.get_or_insert(0);
    ui.horizontal(|ui| {
        ui.label("X:");
        changed |= ui.add(egui::DragValue::new(x).range(0..=100_000)).changed();
        ui.label("Y:");
        changed |= ui.add(egui::DragValue::new(y).range(0..=100_000)).changed();
        ui.label("px");
    });
    changed
}

/// Show properties for a text element. Returns true if any value changed.
fn show_text_properties(ui: &mut egui::Ui, props: TextProps, state: &mut AppState) -> bool {
    let TextProps {
        content,
        x,
        y,
        font_name,
        font_weight,
        font_size,
        font_size_unit,
        align,
        rotation,
        flip_h,
        flip_v,
    } = props;
    let mut changed = false;

    ui.label("Text Content:");
    let response = ui.add(
        egui::TextEdit::multiline(content)
            .desired_width(f32::INFINITY)
            .desired_rows(4),
    );
    if response.changed() {
        changed = true;
    }

    if state.layout == LayoutMode::Positioned {
        ui.add_space(4.0);
        changed |= show_position(ui, x, y);
    }

    ui.add_space(8.0);

    // Font name (searchable dropdown)
    ui.label("Font:");
    ui.add(
        egui::TextEdit::singleline(&mut state.font_search)
            .desired_width(f32::INFINITY)
            .hint_text("Search fonts..."),
    );
    let query = state.font_search.to_lowercase();
    let selected_font = if state.layout == LayoutMode::Positioned {
        font_name.as_deref().unwrap_or(&state.font_name).to_string()
    } else {
        state.font_name.clone()
    };
    egui::ComboBox::from_id_salt("font_selector")
        .selected_text(&selected_font)
        .width(150.0)
        .height(300.0)
        .show_ui(ui, |ui| {
            for font in &state.available_fonts {
                if !query.is_empty() && !font.to_lowercase().contains(&query) {
                    continue;
                }
                if ui.selectable_label(selected_font == *font, font).clicked() {
                    if state.layout == LayoutMode::Positioned {
                        *font_name = Some(font.clone());
                    } else {
                        state.font_name = font.clone();
                    }
                    state.font_search.clear();
                    changed = true;
                }
            }
        });

    if state.layout == LayoutMode::Positioned {
        if font_weight.is_none() {
            changed = true;
        }
        let weight = font_weight.get_or_insert(400);
        ui.horizontal(|ui| {
            ui.label("Weight:");
            changed |= ui
                .add(egui::DragValue::new(weight).speed(100).range(1..=1000))
                .changed();
        });
    }

    ui.add_space(4.0);

    // Font size
    let positioned = state.layout == LayoutMode::Positioned;
    if positioned && font_size.is_none() {
        *font_size = Some(12.0);
        changed = true;
    }
    let mut use_auto = font_size.is_none();
    if !positioned && ui.checkbox(&mut use_auto, "Auto font size").changed() {
        if use_auto {
            *font_size = None;
        } else {
            *font_size = Some(24.0);
        }
        changed = true;
    }

    if let Some(size) = font_size {
        ui.horizontal(|ui| {
            ui.label("Size:");
            if ui
                .add(egui::DragValue::new(size).speed(0.5).range(4.0..=200.0))
                .changed()
            {
                changed = true;
            }
            if positioned {
                if font_size_unit.is_none() {
                    changed = true;
                }
                let unit = font_size_unit.get_or_insert(FontSizeUnit::Points);
                egui::ComboBox::from_id_salt("font_size_unit")
                    .selected_text(match unit {
                        FontSizeUnit::Points => "pt",
                        FontSizeUnit::Pixels => "px",
                    })
                    .show_ui(ui, |ui| {
                        changed |= ui
                            .selectable_value(unit, FontSizeUnit::Points, "pt")
                            .changed();
                        changed |= ui
                            .selectable_value(unit, FontSizeUnit::Pixels, "px")
                            .changed();
                    });
            }
        });
    }

    ui.add_space(4.0);

    if !positioned {
        ui.horizontal(|ui| {
            ui.label("Margin:");
            if ui
                .add(
                    egui::DragValue::new(&mut state.font_margin)
                        .speed(1.0)
                        .range(0..=50),
                )
                .changed()
            {
                changed = true;
            }
            ui.label("px");
        });
    }

    ui.add_space(4.0);

    if !positioned {
        // Alignment controls the horizontal placement within a flow segment.
        ui.label("Alignment:");
        ui.horizontal(|ui| {
            if ui
                .selectable_label(matches!(align, TextAlign::Left), "Left")
                .clicked()
            {
                *align = TextAlign::Left;
                changed = true;
            }
            if ui
                .selectable_label(matches!(align, TextAlign::Center), "Center")
                .clicked()
            {
                *align = TextAlign::Center;
                changed = true;
            }
            if ui
                .selectable_label(matches!(align, TextAlign::Right), "Right")
                .clicked()
            {
                *align = TextAlign::Right;
                changed = true;
            }
        });
        ui.add_space(4.0);
    }

    // Positioned text intentionally stays axis-aligned so glyph coverage can
    // remain grayscale through the anisotropic target transform.
    if positioned {
        if rotation.abs() >= f32::EPSILON {
            changed = true;
        }
        *rotation = 0.0;
        ui.add_space(4.0);
        changed |= show_flip_controls(ui, flip_h, flip_v);
        return changed;
    }

    // Rotation
    ui.label("Rotation:");
    ui.horizontal(|ui| {
        if ui
            .add(
                egui::DragValue::new(rotation)
                    .speed(1.0)
                    .range(-360.0..=360.0)
                    .suffix(" deg"),
            )
            .changed()
        {
            state.rotation_input = format!("{}", *rotation as i32);
            changed = true;
        }
    });
    ui.horizontal(|ui| {
        for &deg in &[0.0_f32, 45.0, 90.0, 135.0, 180.0, 270.0] {
            if ui
                .selectable_label((*rotation - deg).abs() < 0.5, format!("{}", deg as i32))
                .clicked()
            {
                *rotation = deg;
                state.rotation_input = format!("{}", deg as i32);
                changed = true;
            }
        }
    });

    // Manual angle input field
    ui.horizontal(|ui| {
        ui.label("Angle:");
        let resp = ui.add(
            egui::TextEdit::singleline(&mut state.rotation_input)
                .desired_width(60.0)
                .hint_text("deg"),
        );
        if resp.lost_focus()
            && ui.input(|i| i.key_pressed(egui::Key::Enter))
            && let Ok(val) = state.rotation_input.parse::<f32>()
        {
            let clamped = val.clamp(-360.0, 360.0);
            *rotation = clamped;
            state.rotation_input = format!("{}", clamped);
            changed = true;
        }
    });

    ui.add_space(4.0);
    changed |= show_flip_controls(ui, flip_h, flip_v);

    changed
}

/// Read and decode an image file, updating the embedded bytes and render cache.
/// Returns true on success.
fn load_image_into(
    path: &Path,
    image_data: &mut Vec<u8>,
    bitmap: &mut Option<ptouch_render::bitmap::LabelBitmap>,
) -> bool {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) => {
            error!("Image read failed: {}", e);
            return false;
        }
    };
    match image_loader::load_image_from_reader(
        Cursor::new(&bytes),
        &image_loader::ImageLoadOptions::default(),
    ) {
        Ok(bmp) => {
            *image_data = bytes;
            *bitmap = Some(bmp);
            true
        }
        Err(e) => {
            error!("Image decode failed: {}", e);
            false
        }
    }
}

/// Show properties for an image element. Returns true if changed.
fn show_image_properties(ui: &mut egui::Ui, props: ImageProps, state: &mut AppState) -> bool {
    let ImageProps {
        path,
        image_data,
        bitmap,
        x,
        y,
        rotation,
        target_height,
        target_width,
        flip_h,
        flip_v,
    } = props;
    let mut changed = false;

    ui.label("Image");
    ui.add_space(4.0);

    if state.layout == LayoutMode::Positioned {
        changed |= show_position(ui, x, y);
        ui.add_space(4.0);
    }

    let file_label = path
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "embedded".to_string());
    ui.label(format!("File: {}", file_label));

    if let &mut Some(ref bmp) = bitmap {
        ui.label(format!("Original: {} x {} px", bmp.width(), bmp.height()));
    }

    ui.add_space(4.0);

    // Reload re-reads the original file when its path is still known.
    if let Some(p) = path.clone()
        && ui.button("Reload").clicked()
        && load_image_into(&p, image_data, bitmap)
    {
        info!("Reloaded image: {}", p.display());
        changed = true;
    }

    if ui.button("Change File...").clicked()
        && let Some(new_path) = crate::widgets::image_file_dialog().pick_file()
        && load_image_into(&new_path, image_data, bitmap)
    {
        info!("Changed image to: {}", new_path.display());
        *path = Some(new_path);
        changed = true;
    }

    ui.add_space(8.0);

    // Height sizing (auto = fit to tape, manual = user-specified)
    let mut use_auto = target_height.is_none();
    if ui.checkbox(&mut use_auto, "Auto height").changed() {
        if use_auto {
            *target_height = None;
        } else {
            *target_height = Some(state.tape_width_px);
        }
        changed = true;
    }

    if let Some(h) = target_height {
        ui.horizontal(|ui| {
            ui.label("Height:");
            if ui
                .add(egui::DragValue::new(h).speed(1.0).range(1..=1000))
                .changed()
            {
                changed = true;
            }
            ui.label("px");
        });
    }

    if state.layout == LayoutMode::Positioned {
        if target_width.is_none() {
            changed = true;
        }
        let width = target_width.get_or_insert_with(|| {
            bitmap
                .as_ref()
                .filter(|bitmap| bitmap.height() > 0)
                .map(|bitmap| {
                    let height = target_height.unwrap_or(state.tape_width_px);
                    ((u64::from(bitmap.width()) * u64::from(height)) / u64::from(bitmap.height()))
                        .max(1) as u32
                })
                .unwrap_or(1)
        });
        ui.horizontal(|ui| {
            ui.label("Width:");
            changed |= ui
                .add(egui::DragValue::new(width).speed(1).range(1..=100_000))
                .changed();
            ui.label("px");
        });
    }

    ui.add_space(4.0);

    if state.layout == LayoutMode::Positioned {
        if rotation.abs() >= f32::EPSILON {
            changed = true;
        }
        *rotation = 0.0;
        ui.add_space(4.0);
        changed |= show_flip_controls(ui, flip_h, flip_v);
        return changed;
    }

    // Rotation
    ui.label("Rotation:");
    ui.horizontal(|ui| {
        if ui
            .add(
                egui::DragValue::new(rotation)
                    .speed(1.0)
                    .range(-360.0..=360.0)
                    .suffix(" deg"),
            )
            .changed()
        {
            state.rotation_input = format!("{}", *rotation as i32);
            changed = true;
        }
    });
    ui.horizontal(|ui| {
        for &deg in &[0.0_f32, 45.0, 90.0, 135.0, 180.0, 270.0] {
            if ui
                .selectable_label((*rotation - deg).abs() < 0.5, format!("{}", deg as i32))
                .clicked()
            {
                *rotation = deg;
                state.rotation_input = format!("{}", deg as i32);
                changed = true;
            }
        }
    });

    // Manual angle input field
    ui.horizontal(|ui| {
        ui.label("Angle:");
        let resp = ui.add(
            egui::TextEdit::singleline(&mut state.rotation_input)
                .desired_width(60.0)
                .hint_text("deg"),
        );
        if resp.lost_focus()
            && ui.input(|i| i.key_pressed(egui::Key::Enter))
            && let Ok(val) = state.rotation_input.parse::<f32>()
        {
            let clamped = val.clamp(-360.0, 360.0);
            *rotation = clamped;
            state.rotation_input = format!("{}", clamped);
            changed = true;
        }
    });

    ui.add_space(4.0);
    changed |= show_flip_controls(ui, flip_h, flip_v);

    changed
}

/// Show properties for a padding element. Returns true if changed.
fn show_padding_properties(ui: &mut egui::Ui, pixels: &mut u32) -> bool {
    let mut changed = false;

    ui.label("Padding");
    ui.add_space(4.0);

    ui.horizontal(|ui| {
        ui.label("Pixels:");
        if ui
            .add(egui::DragValue::new(pixels).speed(1.0).range(1..=500))
            .changed()
        {
            changed = true;
        }
    });

    ui.add_space(4.0);

    // Quick preset buttons
    ui.horizontal(|ui| {
        for &preset in &[5, 10, 20, 50, 100] {
            if ui.button(format!("{}", preset)).clicked() {
                *pixels = preset;
                changed = true;
            }
        }
    });

    changed
}
