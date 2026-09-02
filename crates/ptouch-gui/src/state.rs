// SPDX-License-Identifier: MIT
// SPDX-FileCopyrightText: 2026 Huang Rui <vowstar@gmail.com>

//! Application state for the P-Touch GUI.

use std::sync::mpsc;

use ptouch_core::device::DeviceFlags;
use ptouch_core::protocol::PrintQuality;
use ptouch_render::bitmap::LabelBitmap;
use ptouch_render::document::{LabelDocument, LayoutMode};
use ptouch_render::layout::ElementBounds;

pub use ptouch_render::document::LabelElement;

/// Commands sent from the UI thread to the printer worker.
pub enum PrinterCommand {
    /// Poll for a connected printer (query status only, no init).
    Poll,
    /// Print raster data.
    Print {
        raster_lines: Vec<Vec<u8>>,
        chain_print: bool,
        auto_cut: bool,
        quality: PrintQuality,
    },
    /// Feed tape forward and cut.
    FeedAndCut,
}

/// Responses sent from the printer worker back to the UI thread.
pub enum PrinterResponse {
    /// A printer was found and its status queried.
    Connected {
        model_name: String,
        media_width: u8,
        media_type: String,
        max_px: u16,
        dpi: u16,
        flags: DeviceFlags,
    },
    /// No printer found or previously connected printer lost.
    Disconnected,
    /// Print job completed successfully.
    PrintDone,
    /// Feed and cut completed successfully.
    FeedAndCutDone,
    /// An operation failed.
    Error(String),
}

/// Central application state shared across all panels.
pub struct AppState {
    /// List of label elements in composition order.
    pub elements: Vec<LabelElement>,
    /// Index of the currently selected element, if any.
    pub selected_element: Option<usize>,
    /// Current tape width in millimeters.
    pub tape_width_mm: u8,
    /// Current tape width in pixels (derived from tape_width_mm).
    pub tape_width_px: u32,
    /// Resolution used by logical positions and dimensions in the document.
    pub document_dpi: u16,
    /// Current document layout mode.
    pub layout: LayoutMode,
    /// Minimum label length for positioned layouts.
    pub min_length: u32,
    /// Blank space after the rightmost positioned element.
    pub end_padding: u32,
    /// Font name used for text rendering.
    pub font_name: String,
    /// Font top/bottom margin in pixels.
    pub font_margin: u32,
    /// Mirror the whole composed label left-right (horizontal).
    pub overall_flip_h: bool,
    /// Mirror the whole composed label top-bottom (vertical).
    pub overall_flip_v: bool,
    /// Cached list of available system font family names.
    pub available_fonts: Vec<String>,
    /// The rendered preview bitmap (1-bit).
    pub preview_bitmap: Option<LabelBitmap>,
    /// Exact raster to send to the printer.
    pub printer_bitmap: Option<LabelBitmap>,
    /// Final logical bounds returned by the renderer.
    pub element_bounds: Vec<ElementBounds>,
    /// The preview texture uploaded to the GPU.
    pub preview_texture: Option<egui::TextureHandle>,
    /// Flag indicating the preview needs to be re-rendered.
    pub needs_rerender: bool,
    /// Current zoom level (1.0 = 100%).
    pub zoom: f32,
    /// Whether zoom should auto-fit to the canvas.
    pub zoom_fit: bool,
    /// Printer connection status message.
    pub printer_status: Option<String>,
    /// Detected printer model name.
    pub printer_model: Option<String>,
    /// Status bar message for transient feedback.
    pub status_message: String,
    /// Buffer for manual rotation angle input in properties panel.
    pub rotation_input: String,
    /// Buffer for font search/filter in properties panel.
    pub font_search: String,
    /// Auto-cut after printing. When false, chain print mode (no cut).
    pub auto_cut: bool,
    /// Whether a printer is currently connected (detected by background poll).
    pub printer_connected: bool,
    /// Whether a printer operation (print, feed & cut) is in progress.
    pub operation_in_progress: bool,
    /// Maximum printable pixels of the last connected printer (0 initially).
    pub printer_max_px: u16,
    /// Print resolution of the last connected printer (180 initially).
    /// Kept across disconnects so the canvas does not resize on a
    /// transient USB glitch.
    pub printer_dpi: u16,
    /// Capabilities of the last connected printer.
    pub printer_flags: Option<DeviceFlags>,
    /// Selected print quality for the next print job.
    pub print_quality: PrintQuality,
    /// Channel sender for commands to the printer worker thread.
    pub printer_cmd_tx: Option<mpsc::Sender<PrinterCommand>>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            elements: Vec::new(),
            selected_element: None,
            tape_width_mm: 12,
            tape_width_px: 76,
            document_dpi: 180,
            layout: LayoutMode::Flow,
            min_length: 0,
            end_padding: 0,
            font_name: "DejaVuSans".to_string(),
            font_margin: 0,
            overall_flip_h: false,
            overall_flip_v: false,
            available_fonts: Vec::new(),
            preview_bitmap: None,
            printer_bitmap: None,
            element_bounds: Vec::new(),
            preview_texture: None,
            needs_rerender: true,
            zoom: 1.0,
            zoom_fit: true,
            printer_status: None,
            printer_model: None,
            status_message: "Ready".to_string(),
            rotation_input: String::new(),
            font_search: String::new(),
            auto_cut: true,
            printer_connected: false,
            operation_in_progress: false,
            printer_max_px: 0,
            printer_dpi: 180,
            printer_flags: None,
            print_quality: PrintQuality::Standard,
            printer_cmd_tx: None,
        }
    }
}

impl AppState {
    /// Convert the editor state into its serialized document model.
    pub fn to_document(&self) -> LabelDocument {
        LabelDocument {
            version: if self.layout == LayoutMode::Positioned {
                2
            } else {
                1
            },
            tape_width_mm: self.tape_width_mm,
            dpi: self.document_dpi,
            layout: self.layout,
            min_length: self.min_length,
            end_padding: self.end_padding,
            font_name: self.font_name.clone(),
            font_margin: self.font_margin,
            flip_h: self.overall_flip_h,
            flip_v: self.overall_flip_v,
            elements: self.elements.clone(),
        }
    }

    /// Replace editor fields with a loaded document.
    pub fn apply_document(&mut self, document: LabelDocument) {
        self.tape_width_mm = document.tape_width_mm;
        self.document_dpi = document.dpi;
        self.layout = document.layout;
        self.min_length = document.min_length;
        self.end_padding = document.end_padding;
        self.font_name = document.font_name;
        self.font_margin = document.font_margin;
        self.overall_flip_h = document.flip_h;
        self.overall_flip_v = document.flip_v;
        self.elements = document.elements;
        self.selected_element = None;
    }

    /// Update the tape width in pixels based on the current tape_width_mm
    /// and the connected printer's resolution.
    pub fn update_tape_pixels(&mut self) {
        if let Some(tape) = ptouch_core::tape::find_tape(self.tape_width_mm, self.printer_dpi) {
            let px = u32::from(tape.pixels);
            self.tape_width_px = if self.printer_max_px > 0 {
                px.min(u32::from(self.printer_max_px))
            } else {
                px
            };
        }
    }

    /// Mark the preview as needing re-render.
    pub fn mark_dirty(&mut self) {
        self.needs_rerender = true;
    }

    /// Ensure the selected element index is valid.
    pub fn validate_selection(&mut self) {
        if let Some(idx) = self.selected_element
            && idx >= self.elements.len()
        {
            self.selected_element = if self.elements.is_empty() {
                None
            } else {
                Some(self.elements.len() - 1)
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use ptouch_render::document::{FontSizeUnit, LayoutMode};
    use ptouch_render::text::TextAlign;

    use super::*;

    #[test]
    fn positioned_document_round_trip_preserves_editor_fields() {
        let mut state = AppState {
            layout: LayoutMode::Positioned,
            min_length: 230,
            end_padding: 3,
            ..AppState::default()
        };
        for (index, weight) in [700, 300, 500, 800].into_iter().enumerate() {
            state.elements.push(LabelElement::Text {
                content: format!("{{{{field_{index}}}}}"),
                x: Some(141),
                y: Some([3, 28, 52, 85][index]),
                font_name: Some("Inter".to_string()),
                font_weight: Some(weight),
                font_size: Some(if index == 3 { 14.0 } else { 8.0 }),
                font_size_unit: Some(FontSizeUnit::Points),
                align: TextAlign::Left,
                rotation: 0.0,
                flip_h: false,
                flip_v: false,
            });
        }

        let serialized = state.to_document().to_toml_string().unwrap();
        let document = LabelDocument::from_toml_str(&serialized).unwrap();
        assert_eq!(document.version, 2);
        assert_eq!(document.layout, LayoutMode::Positioned);
        let weights: Vec<Option<u16>> = document
            .elements
            .iter()
            .map(|element| match element {
                LabelElement::Text { font_weight, .. } => *font_weight,
                _ => None,
            })
            .collect();
        assert_eq!(weights, vec![Some(700), Some(300), Some(500), Some(800)]);

        let mut restored = AppState::default();
        restored.apply_document(document);
        assert_eq!(restored.layout, LayoutMode::Positioned);
        assert_eq!(restored.min_length, 230);
        assert_eq!(restored.end_padding, 3);
        match &restored.elements[0] {
            LabelElement::Text {
                x,
                y,
                font_name,
                font_weight,
                font_size_unit,
                ..
            } => {
                assert_eq!((*x, *y), (Some(141), Some(3)));
                assert_eq!(font_name.as_deref(), Some("Inter"));
                assert_eq!(*font_weight, Some(700));
                assert_eq!(*font_size_unit, Some(FontSizeUnit::Points));
            }
            _ => panic!("expected text element"),
        }
    }
}
