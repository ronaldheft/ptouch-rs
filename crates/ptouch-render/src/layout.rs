// SPDX-License-Identifier: MIT
// SPDX-FileCopyrightText: 2026 Huang Rui <vowstar@gmail.com>

//! Unified rendering for flow and positioned label documents.

use crate::bitmap::LabelBitmap;
use image::GrayImage;

use crate::document::{LabelDocument, LabelElement, LayoutMode};
use crate::text::TextRenderer;
use crate::{RenderError, Result};

/// Rectangle in logical document pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ElementBounds {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Logical dimensions of the rendered label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LabelDimensions {
    pub width: u32,
    pub height: u32,
}

/// All render products derived from one document and runtime target.
#[derive(Debug, Clone)]
pub struct RenderedLabel {
    /// Exact anisotropic raster sent to the printer.
    pub printer_raster: LabelBitmap,
    /// Square-pixel representation of the physical print.
    pub preview: LabelBitmap,
    /// Final label dimensions in logical document pixels.
    pub logical_dimensions: LabelDimensions,
    /// Final logical bounds in document element order.
    pub element_bounds: Vec<ElementBounds>,
}

enum PositionedElement {
    Image {
        source: LabelBitmap,
        bounds: ElementBounds,
        flip_h: bool,
        flip_v: bool,
    },
    Text {
        coverage: GrayImage,
        bounds: ElementBounds,
        flip_h: bool,
        flip_v: bool,
    },
}

/// Render a document for a specific runtime printer target.
pub fn render_positioned_document(
    document: &LabelDocument,
    tape_width_px: u32,
) -> Result<RenderedLabel> {
    document.validate_version()?;
    if document.dpi == 0 {
        return Err(RenderError::Layout(
            "document and target resolutions must be greater than zero".to_string(),
        ));
    }
    if document.layout != LayoutMode::Positioned {
        return Err(RenderError::Layout("expected a positioned document".into()));
    }

    let logical_height = tape_width_px;
    let mut bounds = Vec::with_capacity(document.elements.len());
    let mut positioned = Vec::with_capacity(document.elements.len());
    let mut right_edge = 0;
    let mut text_renderer = TextRenderer::new();

    for (element_index, element) in document.elements.iter().enumerate() {
        let rendered = match element {
            LabelElement::Image {
                bitmap,
                x,
                y,
                rotation,
                target_width,
                target_height,
                flip_h,
                flip_v,
                ..
            } => {
                require_no_rotation(*rotation, "image")?;
                let source = bitmap.as_ref().ok_or_else(|| {
                    RenderError::Layout("positioned image has no decoded bitmap".to_string())
                })?;
                let x = required_coordinate(*x, "image", "x")?;
                let y = required_coordinate(*y, "image", "y")?;
                let height = target_height.unwrap_or(logical_height);
                let width = match target_width {
                    Some(width) => *width,
                    None if source.height() > 0 => ((u64::from(source.width()) * u64::from(height))
                        / u64::from(source.height()))
                    .max(1) as u32,
                    None => 0,
                };
                PositionedElement::Image {
                    source: source.clone(),
                    bounds: ElementBounds {
                        x,
                        y,
                        width,
                        height,
                    },
                    flip_h: *flip_h,
                    flip_v: *flip_v,
                }
            }
            LabelElement::Text {
                content,
                x,
                y,
                font_size,
                rotation,
                flip_h,
                flip_v,
                ..
            } => {
                let x = required_coordinate(*x, "text", "x")?;
                let y = required_coordinate(*y, "text", "y")?;
                if content.is_empty() {
                    bounds.push(ElementBounds {
                        x,
                        y,
                        width: 0,
                        height: 0,
                    });
                    continue;
                }
                require_no_rotation(*rotation, "text")?;
                let size = font_size.ok_or_else(|| {
                    RenderError::Layout("positioned text requires font_size".to_string())
                })?;
                let logical_font_size = size * f32::from(document.dpi) / 72.0;
                let lines: Vec<&str> = content.lines().collect();
                let coverage = text_renderer.render_text_grayscale(
                    &lines,
                    &document.font_name,
                    logical_font_size,
                    400,
                )?;
                PositionedElement::Text {
                    bounds: ElementBounds {
                        x,
                        y,
                        width: coverage.width(),
                        height: coverage.height(),
                    },
                    coverage,
                    flip_h: *flip_h,
                    flip_v: *flip_v,
                }
            }
            LabelElement::CutMark | LabelElement::Padding { .. } => {
                return Err(RenderError::Layout(
                    "cut marks and padding are only supported by flow layouts".to_string(),
                ));
            }
        };
        let element_bounds = match &rendered {
            PositionedElement::Image { bounds, .. } | PositionedElement::Text { bounds, .. } => {
                *bounds
            }
        };
        let element_kind = match element {
            LabelElement::Image { .. } => "image",
            LabelElement::Text { .. } => "text",
            LabelElement::CutMark | LabelElement::Padding { .. } => unreachable!(),
        };
        let (raster_bounds, element_right_edge) =
            positioned_raster_bounds(element_bounds, tape_width_px, element_index, element_kind)?;
        right_edge = right_edge.max(element_right_edge);
        bounds.push(element_bounds);
        positioned.push((rendered, raster_bounds));
    }

    let padded_right_edge = right_edge
        .checked_add(document.end_padding)
        .ok_or_else(|| RenderError::Layout("positioned label length is too large".to_string()))?;
    let logical_width = document.min_length.max(padded_right_edge);
    let mut printer_raster = LabelBitmap::new(logical_width, tape_width_px);
    for (element, raster_bounds) in positioned {
        let (image, bounds) = match element {
            PositionedElement::Image {
                source,
                flip_h,
                flip_v,
                ..
            } => (
                source
                    .scale_to_size(raster_bounds.width, raster_bounds.height)
                    .mirrored(flip_h, flip_v),
                raster_bounds,
            ),
            PositionedElement::Text {
                coverage,
                flip_h,
                flip_v,
                ..
            } => {
                let coverage = image::imageops::resize(
                    &coverage,
                    raster_bounds.width,
                    raster_bounds.height,
                    image::imageops::FilterType::Triangle,
                );
                (
                    LabelBitmap::from_gray_image(&coverage, 127).mirrored(flip_h, flip_v),
                    raster_bounds,
                )
            }
        };
        blit(&mut printer_raster, &image, bounds.x, bounds.y);
    }

    if document.flip_h || document.flip_v {
        printer_raster = printer_raster.mirrored(document.flip_h, document.flip_v);
    }
    let preview = printer_raster.clone();

    Ok(RenderedLabel {
        printer_raster,
        preview,
        logical_dimensions: LabelDimensions {
            width: logical_width,
            height: logical_height,
        },
        element_bounds: bounds,
    })
}

fn required_coordinate(value: Option<u32>, element: &str, axis: &str) -> Result<u32> {
    value.ok_or_else(|| RenderError::Layout(format!("positioned {element} requires {axis}")))
}

/// Map both edges of a positioned element to the target raster and reject a
/// bottom edge beyond the printer's actual cross-tape print area. Scaling the
/// edges (rather than the origin and size independently) keeps a valid element
/// whose logical bottom edge is exactly on the boundary exactly on the raster
/// boundary after rounding.
fn positioned_raster_bounds(
    bounds: ElementBounds,
    tape_width_px: u32,
    index: usize,
    kind: &str,
) -> Result<(ElementBounds, u32)> {
    let right = bounds
        .x
        .checked_add(bounds.width)
        .ok_or_else(|| RenderError::Layout("positioned width overflow".into()))?;
    let bottom = bounds
        .y
        .checked_add(bounds.height)
        .ok_or_else(|| RenderError::Layout("positioned height overflow".into()))?;
    if bounds.width == 0 || bounds.height == 0 || bottom > tape_width_px {
        return Err(RenderError::Layout(format!(
            "element {} ({kind}) does not fit printable height {tape_width_px}",
            index + 1
        )));
    }
    Ok((bounds, right))
}

fn require_no_rotation(rotation: f32, element: &str) -> Result<()> {
    if effectively_unrotated(rotation) {
        Ok(())
    } else {
        Err(RenderError::Layout(format!(
            "positioned {element} rotation is not supported"
        )))
    }
}

fn effectively_unrotated(rotation: f32) -> bool {
    let normalized = rotation.rem_euclid(360.0);
    normalized < 0.5 || (360.0 - normalized) < 0.5
}

fn blit(destination: &mut LabelBitmap, source: &LabelBitmap, x: u32, y: u32) {
    for source_y in 0..source.height() {
        for source_x in 0..source.width() {
            if source.get_pixel(source_x, source_y) {
                destination.set_pixel(x + source_x, y + source_y, true);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn doc(y: u32) -> LabelDocument {
        LabelDocument::from_toml_str(&format!("version = 2\ntape_width_mm = 24\ndpi = 180\nlayout = \"positioned\"\nmin_length = 80\nend_padding = 3\nfont_name = \"sans-serif\"\nfont_margin = 0\n[[elements]]\ntype = \"text\"\ncontent = \"A\"\nx = 10\ny = {y}\nfont_size = 7\n")).unwrap()
    }
    #[test]
    fn positioned_bounds_determine_length_and_reject_clipping() {
        let result = render_positioned_document(&doc(12), 128).unwrap();
        assert_eq!(result.element_bounds[0].x, 10);
        assert_eq!(result.element_bounds[0].y, 12);
        assert_eq!(
            result.printer_raster.width(),
            80.max(13 + result.element_bounds[0].width)
        );
        let height = result.element_bounds[0].height;
        assert!(render_positioned_document(&doc(128 - height), 128).is_ok());
        assert!(render_positioned_document(&doc(129 - height), 128).is_err());
    }
}
