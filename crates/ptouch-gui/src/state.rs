// SPDX-License-Identifier: MIT
// SPDX-FileCopyrightText: 2026 Huang Rui <vowstar@gmail.com>

//! Application state for the P-Touch GUI.

use std::sync::mpsc;

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
        quality_modes: bool,
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
    /// Font name used for text rendering.
    pub font_name: String,
    /// Resolution used by the saved layout coordinates.
    pub document_dpi: u16,
    pub layout: LayoutMode,
    pub min_length: u32,
    pub end_padding: u32,
    pub element_bounds: Vec<ElementBounds>,
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
    /// Whether the last connected printer supports print quality modes.
    pub printer_quality_modes: bool,
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
            font_name: "DejaVuSans".to_string(),
            document_dpi: 180,
            layout: LayoutMode::Flow,
            min_length: 0,
            end_padding: 0,
            element_bounds: Vec::new(),
            font_margin: 0,
            overall_flip_h: false,
            overall_flip_v: false,
            available_fonts: Vec::new(),
            preview_bitmap: None,
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
            printer_quality_modes: false,
            print_quality: PrintQuality::Standard,
            printer_cmd_tx: None,
        }
    }
}

impl AppState {
    /// Switching layout modes must not discard flow-only elements or rotations.
    pub fn can_use_positioned_layout(&self) -> bool {
        self.elements.iter().all(|element| match element {
            LabelElement::Text { rotation, .. } | LabelElement::Image { rotation, .. } => {
                let angle = rotation.rem_euclid(360.0);
                angle < 0.5 || (360.0 - angle) < 0.5
            }
            LabelElement::CutMark | LabelElement::Padding { .. } => false,
        })
    }

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

    /// Use the document's design resolution until a printer supplies geometry.
    pub fn render_dpi(&self) -> u16 {
        if self.printer_connected {
            self.printer_dpi
        } else {
            self.document_dpi
        }
    }

    /// Update the tape width in pixels based on the current tape_width_mm
    /// and the connected printer's resolution.
    pub fn update_tape_pixels(&mut self) {
        if let Some(tape) = ptouch_core::tape::find_tape(self.tape_width_mm, self.render_dpi()) {
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
    use super::*;
    #[test]
    fn positioned_editor_roundtrip_retains_coordinates_and_dimensions() {
        let doc = LabelDocument::from_toml_str("version = 2\ntape_width_mm = 24\ndpi = 180\nlayout = \"positioned\"\nmin_length = 230\nend_padding = 3\nfont_name = \"sans-serif\"\nfont_margin = 0\n[[elements]]\ntype = \"text\"\ncontent = \"Sample\"\nx = 141\ny = 12\nfont_size = 7\n").unwrap();
        let mut state = AppState::default();
        state.apply_document(doc);
        let saved = state.to_document();
        let reopened = LabelDocument::from_toml_str(&saved.to_toml_string().unwrap()).unwrap();
        assert_eq!(reopened.layout, LayoutMode::Positioned);
        assert_eq!((reopened.min_length, reopened.end_padding), (230, 3));
        assert!(matches!(
            &reopened.elements[0],
            LabelElement::Text {
                x: Some(141),
                y: Some(12),
                ..
            }
        ));
    }
    #[test]
    fn switching_layout_mode_does_not_discard_rotated_content() {
        let mut state = AppState::default();
        state.elements.push(LabelElement::Text {
            content: "Rotated".into(),
            x: None,
            y: None,
            font_size: Some(12.0),
            align: ptouch_render::text::TextAlign::Left,
            rotation: 90.0,
            flip_h: false,
            flip_v: false,
        });
        assert!(!state.can_use_positioned_layout());
        assert!(matches!(
            &state.elements[0],
            LabelElement::Text { rotation: 90.0, .. }
        ));
        state.elements.clear();
        state.elements.push(LabelElement::CutMark);
        assert!(!state.can_use_positioned_layout());
    }
}
