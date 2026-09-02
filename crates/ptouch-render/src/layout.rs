// SPDX-License-Identifier: MIT
// SPDX-FileCopyrightText: 2026 Huang Rui <vowstar@gmail.com>

//! Unified rendering for flow and positioned label documents.

use crate::bitmap::LabelBitmap;
use image::GrayImage;
use log::error;
use ptouch_core::device::DeviceFlags;
use ptouch_core::protocol::{PrintQuality, render_feed_scale};

use crate::document::{FontSizeUnit, LabelDocument, LabelElement, LayoutMode};
use crate::text::TextRenderer;
use crate::{RenderError, Result};

/// Runtime printer geometry used to render a document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderTarget {
    /// Printable dots across the tape.
    pub tape_width_px: u32,
    /// Resolution across the tape.
    pub cross_dpi: u16,
    /// Resolution along the tape feed direction.
    pub feed_dpi: u16,
}

impl RenderTarget {
    /// Build target geometry from print quality and optional device capabilities.
    pub fn for_print_quality(
        tape_width_px: u32,
        cross_dpi: u16,
        quality: PrintQuality,
        flags: Option<DeviceFlags>,
    ) -> Self {
        Self {
            tape_width_px,
            cross_dpi,
            feed_dpi: cross_dpi.saturating_mul(render_feed_scale(quality, flags)),
        }
    }
}

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
pub fn render_document(document: &LabelDocument, target: RenderTarget) -> Result<RenderedLabel> {
    if document.dpi == 0 || target.cross_dpi == 0 || target.feed_dpi == 0 {
        return Err(RenderError::Layout(
            "document and target resolutions must be greater than zero".to_string(),
        ));
    }
    if document.layout == LayoutMode::Positioned && document.version < 2 {
        return Err(RenderError::Layout(
            "positioned layout requires layout version 2".to_string(),
        ));
    }
    if document.layout == LayoutMode::Flow {
        return render_flow_document(document, target);
    }

    let logical_height = scale(target.tape_width_px, target.cross_dpi, document.dpi);
    let mut bounds = Vec::with_capacity(document.elements.len());
    let mut positioned = Vec::with_capacity(document.elements.len());
    let mut right_edge = 0;
    let mut text_renderer = TextRenderer::new();

    for element in &document.elements {
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
                font_name,
                font_weight,
                font_size,
                font_size_unit,
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
                let logical_font_size = match font_size_unit.unwrap_or(FontSizeUnit::Points) {
                    FontSizeUnit::Points => size * f32::from(document.dpi) / 72.0,
                    FontSizeUnit::Pixels => size,
                };
                let lines: Vec<&str> = content.lines().collect();
                let coverage = text_renderer.render_text_grayscale(
                    &lines,
                    font_name.as_deref().unwrap_or(&document.font_name),
                    logical_font_size,
                    font_weight.unwrap_or(400),
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
        right_edge = right_edge.max(element_bounds.x.saturating_add(element_bounds.width));
        bounds.push(element_bounds);
        positioned.push(rendered);
    }

    let logical_width = document
        .min_length
        .max(right_edge.saturating_add(document.end_padding));
    let mut printer_raster = LabelBitmap::new(
        scale(logical_width, document.dpi, target.feed_dpi),
        target.tape_width_px,
    );
    for element in positioned {
        let (image, bounds) = match element {
            PositionedElement::Image {
                source,
                bounds,
                flip_h,
                flip_v,
            } => (
                source
                    .scale_to_size(
                        scale(bounds.width, document.dpi, target.feed_dpi),
                        scale(bounds.height, document.dpi, target.cross_dpi),
                    )
                    .mirrored(flip_h, flip_v),
                bounds,
            ),
            PositionedElement::Text {
                coverage,
                bounds,
                flip_h,
                flip_v,
            } => {
                let coverage = image::imageops::resize(
                    &coverage,
                    scale(bounds.width, document.dpi, target.feed_dpi),
                    scale(bounds.height, document.dpi, target.cross_dpi),
                    image::imageops::FilterType::Triangle,
                );
                (
                    LabelBitmap::from_gray_image(&coverage, 127).mirrored(flip_h, flip_v),
                    bounds,
                )
            }
        };
        blit(
            &mut printer_raster,
            &image,
            scale(bounds.x, document.dpi, target.feed_dpi),
            scale(bounds.y, document.dpi, target.cross_dpi),
        );
    }

    if document.flip_h || document.flip_v {
        printer_raster = printer_raster.mirrored(document.flip_h, document.flip_v);
    }
    let preview = physical_preview(&printer_raster, target);

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

fn render_flow_document(document: &LabelDocument, target: RenderTarget) -> Result<RenderedLabel> {
    let logical_height = scale(target.tape_width_px, target.cross_dpi, document.dpi);
    let mut renderer = TextRenderer::new();
    let mut printer_raster: Option<LabelBitmap> = None;
    let mut element_bounds = Vec::with_capacity(document.elements.len());
    let mut logical_x = 0;

    for element in &document.elements {
        if target.feed_dpi != target.cross_dpi
            && let LabelElement::Text {
                content,
                font_size,
                align,
                rotation,
                flip_h,
                flip_v,
                ..
            } = element
            && effectively_unrotated(*rotation)
        {
            if content.is_empty() {
                element_bounds.push(ElementBounds {
                    x: logical_x,
                    y: 0,
                    width: 0,
                    height: logical_height,
                });
                continue;
            }
            let lines: Vec<&str> = content.lines().collect();
            let coverage = match renderer.render_flow_text_grayscale(
                &lines,
                target.tape_width_px,
                &document.font_name,
                *font_size,
                document.font_margin,
                *align,
            ) {
                Ok(coverage) => coverage,
                Err(error) => {
                    error!("Text render failed: {error}");
                    element_bounds.push(ElementBounds {
                        x: logical_x,
                        y: 0,
                        width: 0,
                        height: logical_height,
                    });
                    continue;
                }
            };
            let coverage = image::imageops::resize(
                &coverage,
                scale(coverage.width(), target.cross_dpi, target.feed_dpi),
                coverage.height(),
                image::imageops::FilterType::Triangle,
            );
            let printer_segment =
                LabelBitmap::from_gray_image(&coverage, 127).mirrored(*flip_h, *flip_v);
            let logical_width = scale(printer_segment.width(), target.feed_dpi, document.dpi);
            element_bounds.push(ElementBounds {
                x: logical_x,
                y: 0,
                width: logical_width,
                height: logical_height,
            });
            logical_x = logical_x.saturating_add(logical_width);
            printer_raster = Some(match printer_raster {
                Some(previous) => previous.append(&printer_segment),
                None => printer_segment,
            });
            continue;
        }

        let segment = crate::document::render_elements(
            std::slice::from_ref(element),
            target.tape_width_px,
            &document.font_name,
            document.font_margin,
            &mut renderer,
        )?;
        let Some(segment) = segment else {
            element_bounds.push(ElementBounds {
                x: logical_x,
                y: 0,
                width: 0,
                height: logical_height,
            });
            continue;
        };
        let logical_width = scale(segment.width(), target.cross_dpi, document.dpi);
        element_bounds.push(ElementBounds {
            x: logical_x,
            y: 0,
            width: logical_width,
            height: logical_height,
        });
        logical_x = logical_x.saturating_add(logical_width);

        let printer_segment = segment.scale_to_size(
            scale(logical_width, document.dpi, target.feed_dpi),
            target.tape_width_px,
        );
        printer_raster = Some(match printer_raster {
            Some(previous) => previous.append(&printer_segment),
            None => printer_segment,
        });
    }

    let mut printer_raster = printer_raster
        .ok_or_else(|| RenderError::Layout("layout produced no output".to_string()))?;
    if document.flip_h || document.flip_v {
        printer_raster = printer_raster.mirrored(document.flip_h, document.flip_v);
    }
    let preview = physical_preview(&printer_raster, target);

    Ok(RenderedLabel {
        printer_raster,
        preview,
        logical_dimensions: LabelDimensions {
            width: logical_x,
            height: logical_height,
        },
        element_bounds,
    })
}

fn physical_preview(printer_raster: &LabelBitmap, target: RenderTarget) -> LabelBitmap {
    let preview_height = scale(printer_raster.height(), target.cross_dpi, target.feed_dpi);
    printer_raster.scale_to_size(printer_raster.width(), preview_height)
}

fn required_coordinate(value: Option<u32>, element: &str, axis: &str) -> Result<u32> {
    value.ok_or_else(|| RenderError::Layout(format!("positioned {element} requires {axis}")))
}

fn require_no_rotation(rotation: f32, element: &str) -> Result<()> {
    if rotation.abs() < f32::EPSILON {
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

fn scale(value: u32, from_dpi: u16, to_dpi: u16) -> u32 {
    ((u64::from(value) * u64::from(to_dpi) + u64::from(from_dpi) / 2) / u64::from(from_dpi)) as u32
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
    use std::collections::BTreeMap;

    use crate::document::{FontSizeUnit, LabelElement, LayoutMode};
    use crate::text::TextAlign;

    use super::*;

    #[test]
    fn positioned_image_uses_coordinates_and_minimum_length() {
        let mut source = LabelBitmap::new(4, 4);
        for y in 0..4 {
            for x in 0..4 {
                source.set_pixel(x, y, true);
            }
        }
        let image = LabelElement::Image {
            path: None,
            image_data: Vec::new(),
            bitmap: Some(source),
            x: Some(2),
            y: Some(1),
            rotation: 0.0,
            target_width: Some(4),
            target_height: Some(4),
            flip_h: false,
            flip_v: false,
        };
        let document = LabelDocument {
            version: 2,
            tape_width_mm: 24,
            dpi: 180,
            layout: LayoutMode::Positioned,
            min_length: 10,
            end_padding: 1,
            font_name: "sans-serif".to_string(),
            font_margin: 0,
            flip_h: false,
            flip_v: false,
            elements: vec![image],
        };

        let rendered = render_document(
            &document,
            RenderTarget {
                tape_width_px: 8,
                cross_dpi: 180,
                feed_dpi: 180,
            },
        )
        .unwrap();

        assert_eq!(
            rendered.logical_dimensions,
            LabelDimensions {
                width: 10,
                height: 8
            }
        );
        assert_eq!(
            rendered.element_bounds,
            vec![ElementBounds {
                x: 2,
                y: 1,
                width: 4,
                height: 4,
            }]
        );
        assert_eq!(
            (
                rendered.printer_raster.width(),
                rendered.printer_raster.height()
            ),
            (10, 8)
        );
        assert!(rendered.printer_raster.get_pixel(2, 1));
        assert!(rendered.printer_raster.get_pixel(5, 4));
        assert!(!rendered.printer_raster.get_pixel(6, 4));
    }

    #[test]
    fn renderer_rejects_programmatic_version_one_positioned_documents() {
        let document = LabelDocument {
            version: 1,
            tape_width_mm: 24,
            dpi: 180,
            layout: LayoutMode::Positioned,
            min_length: 10,
            end_padding: 0,
            font_name: "sans-serif".to_string(),
            font_margin: 0,
            flip_h: false,
            flip_v: false,
            elements: Vec::new(),
        };

        let error = render_document(
            &document,
            RenderTarget {
                tape_width_px: 128,
                cross_dpi: 180,
                feed_dpi: 180,
            },
        )
        .unwrap_err();

        assert!(error.to_string().contains("requires layout version 2"));
    }

    #[test]
    fn high_resolution_image_is_pixel_exact_and_preview_is_physically_square() {
        let mut source = LabelBitmap::new(128, 128);
        for y in 0..128 {
            for x in 0..64 {
                source.set_pixel(x, y, true);
            }
        }
        let document = LabelDocument {
            version: 2,
            tape_width_mm: 24,
            dpi: 180,
            layout: LayoutMode::Positioned,
            min_length: 0,
            end_padding: 0,
            font_name: "sans-serif".to_string(),
            font_margin: 0,
            flip_h: false,
            flip_v: false,
            elements: vec![LabelElement::Image {
                path: None,
                image_data: Vec::new(),
                bitmap: Some(source),
                x: Some(0),
                y: Some(0),
                rotation: 0.0,
                target_width: Some(128),
                target_height: Some(128),
                flip_h: false,
                flip_v: false,
            }],
        };

        let rendered = render_document(
            &document,
            RenderTarget {
                tape_width_px: 128,
                cross_dpi: 180,
                feed_dpi: 360,
            },
        )
        .unwrap();

        assert_eq!(
            (
                rendered.printer_raster.width(),
                rendered.printer_raster.height()
            ),
            (256, 128)
        );
        assert_eq!(
            (rendered.preview.width(), rendered.preview.height()),
            (256, 256)
        );
        assert!(rendered.printer_raster.get_pixel(127, 64));
        assert!(!rendered.printer_raster.get_pixel(128, 64));
        assert_eq!(
            rendered.preview.get_pixel(127, 128),
            rendered.printer_raster.get_pixel(127, 64)
        );
    }

    #[test]
    fn positioned_text_uses_element_typography_and_anisotropic_target() {
        let document = LabelDocument {
            version: 2,
            tape_width_mm: 24,
            dpi: 180,
            layout: LayoutMode::Positioned,
            min_length: 40,
            end_padding: 3,
            font_name: "serif".to_string(),
            font_margin: 0,
            flip_h: false,
            flip_v: false,
            elements: vec![LabelElement::Text {
                content: "Wide".to_string(),
                x: Some(5),
                y: Some(3),
                font_name: Some("sans-serif".to_string()),
                font_weight: Some(800),
                font_size: Some(8.0),
                font_size_unit: Some(FontSizeUnit::Points),
                align: TextAlign::Left,
                rotation: 0.0,
                flip_h: false,
                flip_v: false,
            }],
        };

        let rendered = render_document(
            &document,
            RenderTarget {
                tape_width_px: 128,
                cross_dpi: 180,
                feed_dpi: 360,
            },
        )
        .unwrap();

        let bounds = rendered.element_bounds[0];
        assert_eq!((bounds.x, bounds.y), (5, 3));
        assert!(bounds.width > 0);
        assert!(bounds.height > 0);
        assert_eq!(rendered.printer_raster.height(), 128);
        assert_eq!(rendered.preview.height(), 256);
        assert!(rendered.printer_raster.data().iter().any(|byte| *byte != 0));
    }

    #[test]
    fn template_substitution_expands_positioned_label_length() {
        let document = LabelDocument {
            version: 2,
            tape_width_mm: 24,
            dpi: 180,
            layout: LayoutMode::Positioned,
            min_length: 20,
            end_padding: 3,
            font_name: "sans-serif".to_string(),
            font_margin: 0,
            flip_h: false,
            flip_v: false,
            elements: vec![LabelElement::Text {
                content: "{{name}}".to_string(),
                x: Some(5),
                y: Some(2),
                font_name: None,
                font_weight: Some(400),
                font_size: Some(12.0),
                font_size_unit: Some(FontSizeUnit::Pixels),
                align: TextAlign::Left,
                rotation: 0.0,
                flip_h: false,
                flip_v: false,
            }],
        };
        let target = RenderTarget {
            tape_width_px: 64,
            cross_dpi: 180,
            feed_dpi: 180,
        };

        let render_with_name = |name: &str| {
            let mut resolved = document.clone();
            resolved.apply_values(&BTreeMap::from([("name".to_string(), name.to_string())]));
            render_document(&resolved, target).unwrap()
        };
        let short = render_with_name("A");
        let long = render_with_name("Alexandria");

        assert_eq!(
            short.logical_dimensions.width,
            20.max(short.element_bounds[0].x + short.element_bounds[0].width + 3)
        );
        assert_eq!(
            long.logical_dimensions.width,
            20.max(long.element_bounds[0].x + long.element_bounds[0].width + 3)
        );
        assert!(long.logical_dimensions.width > short.logical_dimensions.width);
    }

    #[test]
    fn version_one_flow_adapter_preserves_existing_raster() {
        let document = LabelDocument {
            version: 1,
            tape_width_mm: 12,
            dpi: 180,
            layout: LayoutMode::Flow,
            min_length: 0,
            end_padding: 0,
            font_name: "sans-serif".to_string(),
            font_margin: 0,
            flip_h: false,
            flip_v: false,
            elements: vec![LabelElement::Padding { pixels: 10 }, LabelElement::CutMark],
        };
        let target = RenderTarget {
            tape_width_px: 64,
            cross_dpi: 180,
            feed_dpi: 180,
        };
        let rendered = render_document(&document, target).unwrap();
        let mut text_renderer = TextRenderer::new();
        let legacy = crate::document::render_elements(
            &document.elements,
            target.tape_width_px,
            &document.font_name,
            document.font_margin,
            &mut text_renderer,
        )
        .unwrap()
        .unwrap();

        assert_eq!(rendered.printer_raster.data(), legacy.data());
        assert_eq!(
            rendered.element_bounds,
            vec![
                ElementBounds {
                    x: 0,
                    y: 0,
                    width: 10,
                    height: 64,
                },
                ElementBounds {
                    x: 10,
                    y: 0,
                    width: 9,
                    height: 64,
                },
            ]
        );
    }

    #[test]
    fn flow_high_resolution_text_is_scaled_before_thresholding() {
        let document = LabelDocument {
            version: 1,
            tape_width_mm: 12,
            dpi: 180,
            layout: LayoutMode::Flow,
            min_length: 0,
            end_padding: 0,
            font_name: "sans-serif".to_string(),
            font_margin: 2,
            flip_h: false,
            flip_v: false,
            elements: vec![LabelElement::Text {
                content: "Semantic".to_string(),
                x: None,
                y: None,
                font_name: None,
                font_weight: None,
                font_size: Some(20.0),
                font_size_unit: None,
                align: TextAlign::Left,
                rotation: 0.0,
                flip_h: false,
                flip_v: false,
            }],
        };
        let standard = render_document(
            &document,
            RenderTarget {
                tape_width_px: 64,
                cross_dpi: 180,
                feed_dpi: 180,
            },
        )
        .unwrap();
        let high_resolution = render_document(
            &document,
            RenderTarget {
                tape_width_px: 64,
                cross_dpi: 180,
                feed_dpi: 360,
            },
        )
        .unwrap();
        let nearest = standard.printer_raster.scale_to_size(
            standard.printer_raster.width() * 2,
            standard.printer_raster.height(),
        );

        assert_eq!(
            high_resolution.printer_raster.width(),
            standard.printer_raster.width() * 2
        );
        assert_ne!(high_resolution.printer_raster.data(), nearest.data());
    }
}
