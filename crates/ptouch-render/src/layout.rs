// SPDX-License-Identifier: MIT
// SPDX-FileCopyrightText: 2026 Huang Rui <vowstar@gmail.com>

//! Unified rendering for flow and positioned label documents.

use crate::bitmap::LabelBitmap;
use log::error;
use ptouch_core::device::DeviceFlags;
use ptouch_core::protocol::{PrintQuality, render_feed_scale};

use crate::document::{LabelDocument, LabelElement};
use crate::text::TextRenderer;
use crate::{RenderError, Result};

/// Runtime printer geometry used to render a document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderTarget {
    /// Actual printable raster height in dots across the tape.
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

/// Render a flow document at the runtime printer resolution.
pub fn render_document(document: &LabelDocument, target: RenderTarget) -> Result<RenderedLabel> {
    if document.dpi == 0 || target.cross_dpi == 0 || target.feed_dpi == 0 {
        return Err(RenderError::Layout(
            "document and target resolutions must be greater than zero".into(),
        ));
    }
    let logical_height = scale(target.tape_width_px, target.cross_dpi, document.dpi);
    let mut renderer = TextRenderer::new();
    let mut printer_raster: Option<LabelBitmap> = None;
    let mut element_bounds = Vec::with_capacity(document.elements.len());
    let mut logical_x = 0;

    for element in &document.elements {
        // An explicit width describes logical placement, not source sampling.
        // Resize directly from the source so native feed samples survive.
        if let LabelElement::Image {
            bitmap: Some(source),
            target_width: Some(width),
            target_height,
            rotation,
            flip_h,
            flip_v,
            ..
        } = element
        {
            require_no_rotation(*rotation, "image with target_width")?;
            let height = target_height.unwrap_or(logical_height);
            let raster_height = scale(height, document.dpi, target.cross_dpi);
            if *width == 0 || raster_height == 0 || raster_height > target.tape_width_px {
                return Err(RenderError::Layout(
                    "flow image dimensions must fit the printable height".into(),
                ));
            }
            let raster_width = scale(*width, document.dpi, target.feed_dpi);
            let image = source
                .scale_to_size(raster_width, raster_height)
                .mirrored(*flip_h, *flip_v);
            let mut segment = LabelBitmap::new(raster_width, target.tape_width_px);
            blit(
                &mut segment,
                &image,
                0,
                (target.tape_width_px - raster_height) / 2,
            );
            element_bounds.push(ElementBounds {
                x: logical_x,
                y: (logical_height - height) / 2,
                width: *width,
                height,
            });
            logical_x = logical_x.saturating_add(*width);
            printer_raster = Some(match printer_raster {
                Some(previous) => previous.append(&segment),
                None => segment,
            });
            continue;
        }
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
            scale(segment.width(), target.cross_dpi, target.feed_dpi),
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
    use super::*;
    use crate::text::TextAlign;
    #[test]
    fn version_one_flow_adapter_preserves_existing_raster() {
        let document = LabelDocument {
            version: 1,
            tape_width_mm: 12,
            dpi: 180,

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

            font_name: "sans-serif".to_string(),
            font_margin: 2,
            flip_h: false,
            flip_v: false,
            elements: vec![LabelElement::Text {
                content: "Semantic".to_string(),

                font_size: Some(20.0),

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

    #[test]
    fn flow_native_image_preserves_every_feed_sample() {
        let mut source = LabelBitmap::new(466, 128);
        for x in (1..466).step_by(2) {
            source.set_pixel(x, 64, true);
        }
        let mut document = LabelDocument::from_toml_str("version = 1\ntape_width_mm = 24\ndpi = 180\nfont_name = \"sans-serif\"\nfont_margin = 0\nelements = []").unwrap();
        document.elements.push(LabelElement::Image {
            path: None,
            image_data: vec![],
            bitmap: Some(source.clone()),

            rotation: 0.0,
            target_height: Some(128),
            target_width: Some(233),
            flip_h: false,
            flip_v: false,
        });
        let rendered = render_document(
            &document,
            RenderTarget {
                tape_width_px: 128,
                cross_dpi: 180,
                feed_dpi: 360,
            },
        )
        .unwrap();
        assert_eq!(rendered.printer_raster.data(), source.data());
        assert_eq!(rendered.logical_dimensions.width, 233);
        assert_eq!(
            (rendered.preview.width(), rendered.preview.height()),
            (466, 256)
        );
    }
}
