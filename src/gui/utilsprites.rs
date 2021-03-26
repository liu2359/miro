use super::glyphcache::GlyphCache;
use crate::font::FontConfiguration;
use crate::term::Underline;
use crate::window::bitmaps::atlas::{OutOfTextureSpace, Sprite};
use crate::window::bitmaps::{BitmapImage, Image, Texture2d};
use crate::window::*;
use std::rc::Rc;

#[derive(Copy, Clone)]
pub struct RenderMetrics {
    pub descender: PixelLength,
    pub descender_row: IntPixelLength,
    pub descender_plus_two: IntPixelLength,
    pub underline_height: IntPixelLength,
    pub strike_row: IntPixelLength,
    pub cell_size: Size,
}

impl RenderMetrics {
    pub fn new(fonts: &Rc<FontConfiguration>) -> Self {
        let metrics = fonts.default_font_metrics().expect("failed to get font metrics!?");

        let (cell_height, cell_width) =
            (metrics.cell_height.get().ceil() as usize, metrics.cell_width.get().ceil() as usize);

        let underline_height = metrics.underline_thickness.get().round() as isize;

        let descender_row =
            (cell_height as f64 + (metrics.descender - metrics.underline_position).get()) as isize;
        let descender_plus_two =
            (2 * underline_height + descender_row).min(cell_height as isize - 1);
        let strike_row = descender_row / 2;

        Self {
            descender: metrics.descender,
            descender_row,
            descender_plus_two,
            strike_row,
            cell_size: Size::new(cell_width as isize, cell_height as isize),
            underline_height,
        }
    }
}

pub struct UtilSprites<T: Texture2d> {
    pub white_space: Sprite<T>,
    pub single_underline: Sprite<T>,
    pub double_underline: Sprite<T>,
    pub strike_through: Sprite<T>,
    pub single_and_strike: Sprite<T>,
    pub double_and_strike: Sprite<T>,
}

impl<T: Texture2d> UtilSprites<T> {
    pub fn new(
        glyph_cache: &mut GlyphCache<T>,
        metrics: &RenderMetrics,
    ) -> Result<Self, OutOfTextureSpace> {
        let mut buffer =
            Image::new(metrics.cell_size.width as usize, metrics.cell_size.height as usize);

        let black = crate::window::color::Color::rgba(0, 0, 0, 0);
        let white = crate::window::color::Color::rgb(0xff, 0xff, 0xff);

        let cell_rect = Rect::new(Point::new(0, 0), metrics.cell_size);

        buffer.clear_rect(cell_rect, black);
        let white_space = glyph_cache.atlas.allocate(&buffer)?;

        let draw_single = |buffer: &mut Image| {
            for row in 0..metrics.underline_height {
                buffer.draw_line(
                    Point::new(
                        cell_rect.origin.x,
                        cell_rect.origin.y + metrics.descender_row + row,
                    ),
                    Point::new(
                        cell_rect.origin.x + metrics.cell_size.width,
                        cell_rect.origin.y + metrics.descender_row + row,
                    ),
                    white,
                    Operator::Source,
                );
            }
        };

        let draw_double = |buffer: &mut Image| {
            for row in 0..metrics.underline_height {
                buffer.draw_line(
                    Point::new(
                        cell_rect.origin.x,
                        cell_rect.origin.y + metrics.descender_row + row,
                    ),
                    Point::new(
                        cell_rect.origin.x + metrics.cell_size.width,
                        cell_rect.origin.y + metrics.descender_row + row,
                    ),
                    white,
                    Operator::Source,
                );
                buffer.draw_line(
                    Point::new(
                        cell_rect.origin.x,
                        cell_rect.origin.y + metrics.descender_plus_two + row,
                    ),
                    Point::new(
                        cell_rect.origin.x + metrics.cell_size.width,
                        cell_rect.origin.y + metrics.descender_plus_two + row,
                    ),
                    white,
                    Operator::Source,
                );
            }
        };

        let draw_strike = |buffer: &mut Image| {
            for row in 0..metrics.underline_height {
                buffer.draw_line(
                    Point::new(cell_rect.origin.x, cell_rect.origin.y + metrics.strike_row + row),
                    Point::new(
                        cell_rect.origin.x + metrics.cell_size.width,
                        cell_rect.origin.y + metrics.strike_row + row,
                    ),
                    white,
                    Operator::Source,
                );
            }
        };

        buffer.clear_rect(cell_rect, black);
        draw_single(&mut buffer);
        let single_underline = glyph_cache.atlas.allocate(&buffer)?;

        buffer.clear_rect(cell_rect, black);
        draw_double(&mut buffer);
        let double_underline = glyph_cache.atlas.allocate(&buffer)?;

        buffer.clear_rect(cell_rect, black);
        draw_strike(&mut buffer);
        let strike_through = glyph_cache.atlas.allocate(&buffer)?;

        buffer.clear_rect(cell_rect, black);
        draw_single(&mut buffer);
        draw_strike(&mut buffer);
        let single_and_strike = glyph_cache.atlas.allocate(&buffer)?;

        buffer.clear_rect(cell_rect, black);
        draw_double(&mut buffer);
        draw_strike(&mut buffer);
        let double_and_strike = glyph_cache.atlas.allocate(&buffer)?;

        Ok(Self {
            white_space,
            single_underline,
            double_underline,
            strike_through,
            single_and_strike,
            double_and_strike,
        })
    }

    pub fn select_sprite(
        &self,
        is_highlited_hyperlink: bool,
        is_strike_through: bool,
        underline: Underline,
    ) -> &Sprite<T> {
        match (is_highlited_hyperlink, is_strike_through, underline) {
            (true, false, Underline::None) => &self.single_underline,
            (true, false, Underline::Single) => &self.double_underline,
            (true, false, Underline::Double) => &self.single_underline,
            (true, true, Underline::None) => &self.strike_through,
            (true, true, Underline::Single) => &self.single_and_strike,
            (true, true, Underline::Double) => &self.double_and_strike,
            (false, false, Underline::None) => &self.white_space,
            (false, false, Underline::Single) => &self.single_underline,
            (false, false, Underline::Double) => &self.double_underline,
            (false, true, Underline::None) => &self.strike_through,
            (false, true, Underline::Single) => &self.single_and_strike,
            (false, true, Underline::Double) => &self.double_and_strike,
        }
    }
}
