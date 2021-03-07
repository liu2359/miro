#![allow(clippy::cast_lossless)]
use super::hbwrap as harfbuzz;
use crate::font::{ftwrap, Font, FontMetrics, RasterizedGlyph};
use failure::{bail, Error};
use log::debug;
use std::cell::RefCell;
use std::mem;
use std::slice;

/// Holds a loaded font alternative
pub struct FreeTypeFontImpl {
    face: RefCell<ftwrap::Face>,
    #[cfg(all(unix, not(target_os = "macos")))]
    font: RefCell<harfbuzz::Font>,
    /// nominal monospace cell height
    cell_height: f64,
    /// nominal monospace cell width
    cell_width: f64,
}

impl FreeTypeFontImpl {
    pub fn with_face_size_and_dpi(
        mut face: ftwrap::Face,
        size: f64,
        dpi: u32,
    ) -> Result<Self, Error> {
        debug!("set_char_size {} dpi={}", size, dpi);
        // Scaling before truncating to integer minimizes the chances of hitting
        // the fallback code for set_pixel_sizes below.
        let size = (size * 64.0) as ftwrap::FT_F26Dot6;

        let (cell_width, cell_height) = match face.set_char_size(size, size, dpi, dpi) {
            Ok(_) => {
                // Compute metrics for the nominal monospace cell
                face.cell_metrics()
            }
            Err(err) => {
                let sizes = unsafe {
                    let rec = &(*face.face);
                    slice::from_raw_parts(rec.available_sizes, rec.num_fixed_sizes as usize)
                };
                if sizes.is_empty() {
                    return Err(err);
                }
                // Find the best matching size.
                // We just take the biggest.
                let mut best = 0;
                let mut best_size = 0;
                let mut cell_width = 0;
                let mut cell_height = 0;

                for (idx, info) in sizes.iter().enumerate() {
                    let size = best_size.max(info.height);
                    if size > best_size {
                        best = idx;
                        best_size = size;
                        cell_width = info.width;
                        cell_height = info.height;
                    }
                }
                face.select_size(best)?;
                (f64::from(cell_width), f64::from(cell_height))
            }
        };

        debug!("metrics: width={} height={}", cell_width, cell_height);
        #[cfg(all(unix, not(target_os = "macos")))]
        let font = harfbuzz::Font::new(face.face);

        Ok(FreeTypeFontImpl {
            face: RefCell::new(face),
            #[cfg(all(unix, not(target_os = "macos")))]
            font: RefCell::new(font),
            cell_height,
            cell_width,
        })
    }
}

impl Font for FreeTypeFontImpl {
    fn harfbuzz_shape(
        &self,
        buf: &mut harfbuzz::Buffer,
        features: Option<&[harfbuzz::hb_feature_t]>,
    ) {
        #[cfg(all(unix, not(target_os = "macos")))]
        self.font.borrow_mut().shape(buf, features)
    }
    fn has_color(&self) -> bool {
        let face = self.face.borrow();
        unsafe { (((*face.face).face_flags as u32) & (ftwrap::FT_FACE_FLAG_COLOR as u32)) != 0 }
    }

    fn metrics(&self) -> FontMetrics {
        let face = self.face.borrow();
        FontMetrics {
            cell_height: self.cell_height,
            cell_width: self.cell_width,
            // Note: face.face.descender is useless, we have to go through
            // face.face.size.metrics to get to the real descender!
            descender: unsafe { (*(*face.face).size).metrics.descender as f64 } / 64.0,
        }
    }

    fn rasterize_glyph(&self, glyph_pos: u32) -> Result<RasterizedGlyph, Error> {
        let render_mode = //ftwrap::FT_Render_Mode::FT_RENDER_MODE_NORMAL;
 //       ftwrap::FT_Render_Mode::FT_RENDER_MODE_LCD;
        ftwrap::FT_Render_Mode::FT_RENDER_MODE_LIGHT;

        // when changing the load flags, we also need
        // to change them for harfbuzz otherwise it won't
        // hint correctly
        let load_flags = (ftwrap::FT_LOAD_COLOR) as i32 |
            // enable FT_LOAD_TARGET bits.  There are no flags defined
            // for these in the bindings so we do some bit magic for
            // ourselves.  This is how the FT_LOAD_TARGET_() macro
            // assembles these bits.
            (render_mode as i32) << 16;

        #[cfg(all(unix, not(target_os = "macos")))]
        self.font.borrow_mut().set_load_flags(load_flags);

        // This clone is conceptually unsafe, but ok in practice as we are
        // single threaded and don't load any other glyphs in the body of
        // this load_glyph() function.
        let mut face = self.face.borrow_mut();
        let descender = unsafe { (*(*face.face).size).metrics.descender as f64 / 64.0 };
        let ft_glyph = face.load_and_render_glyph(glyph_pos, load_flags, render_mode)?;

        let mode: ftwrap::FT_Pixel_Mode =
            unsafe { mem::transmute(u32::from(ft_glyph.bitmap.pixel_mode)) };

        // pitch is the number of bytes per source row
        let pitch = ft_glyph.bitmap.pitch.abs() as usize;
        let data = unsafe {
            slice::from_raw_parts_mut(ft_glyph.bitmap.buffer, ft_glyph.bitmap.rows as usize * pitch)
        };

        let glyph = match mode {
            ftwrap::FT_Pixel_Mode::FT_PIXEL_MODE_LCD => {
                let width = ft_glyph.bitmap.width as usize / 3;
                let height = ft_glyph.bitmap.rows as usize;
                let size = (width * height * 4) as usize;
                let mut rgba = vec![0u8; size];
                for y in 0..height {
                    let src_offset = y * pitch as usize;
                    let dest_offset = y * width * 4;
                    for x in 0..width {
                        let blue = data[src_offset + (x * 3)];
                        let green = data[src_offset + (x * 3) + 1];
                        let red = data[src_offset + (x * 3) + 2];
                        let alpha = red | green | blue;
                        rgba[dest_offset + (x * 4)] = red;
                        rgba[dest_offset + (x * 4) + 1] = green;
                        rgba[dest_offset + (x * 4) + 2] = blue;
                        rgba[dest_offset + (x * 4) + 3] = alpha;
                    }
                }

                RasterizedGlyph {
                    data: rgba,
                    height,
                    width,
                    bearing_x: ft_glyph.bitmap_left as f64,
                    bearing_y: ft_glyph.bitmap_top as f64,
                }
            }
            ftwrap::FT_Pixel_Mode::FT_PIXEL_MODE_BGRA => {
                let width = ft_glyph.bitmap.width as usize;
                let height = ft_glyph.bitmap.rows as usize;

                // emoji glyphs don't always fill the bitmap size, so we compute
                // the non-transparent bounds here with this simplistic code.
                // This can likely be improved!

                let mut first_line = None;
                let mut first_col = None;
                let mut last_col = None;
                let mut last_line = None;

                for y in 0..height {
                    let src_offset = y * pitch as usize;

                    for x in 0..width {
                        let alpha = data[src_offset + (x * 4) + 3];
                        if alpha != 0 {
                            if first_line.is_none() {
                                first_line = Some(y);
                            }
                            first_col = match first_col.take() {
                                Some(other) if x < other => Some(x),
                                Some(other) => Some(other),
                                None => Some(x),
                            };
                        }
                    }
                }
                for y in (0..height).rev() {
                    let src_offset = y * pitch as usize;

                    for x in (0..width).rev() {
                        let alpha = data[src_offset + (x * 4) + 3];
                        if alpha != 0 {
                            if last_line.is_none() {
                                last_line = Some(y);
                            }
                            last_col = match last_col.take() {
                                Some(other) if x > other => Some(x),
                                Some(other) => Some(other),
                                None => Some(x),
                            };
                        }
                    }
                }

                let first_line = first_line.unwrap_or(0);
                let last_line = last_line.unwrap_or(0);
                let first_col = first_col.unwrap_or(0);
                let last_col = last_col.unwrap_or(0);

                let dest_width = 1 + last_col - first_col;
                let dest_height = 1 + last_line - first_line;

                let size = (dest_width * dest_height * 4) as usize;
                let mut rgba = vec![0u8; size];

                for y in first_line..=last_line {
                    let src_offset = y * pitch as usize;
                    let dest_offset = (y - first_line) * dest_width * 4;
                    for x in first_col..=last_col {
                        let blue = data[src_offset + (x * 4)];
                        let green = data[src_offset + (x * 4) + 1];
                        let red = data[src_offset + (x * 4) + 2];
                        let alpha = data[src_offset + (x * 4) + 3];

                        let dest_x = x - first_col;

                        rgba[dest_offset + (dest_x * 4)] = red;
                        rgba[dest_offset + (dest_x * 4) + 1] = green;
                        rgba[dest_offset + (dest_x * 4) + 2] = blue;
                        rgba[dest_offset + (dest_x * 4) + 3] = alpha;
                    }
                }
                RasterizedGlyph {
                    data: rgba,
                    height: dest_height,
                    width: dest_width,
                    bearing_x: (f64::from(ft_glyph.bitmap_left)
                        * (dest_width as f64 / width as f64)),

                    // Fudge alert: this is font specific: I've found
                    // that the emoji font on macOS doesn't account for the
                    // descender in its metrics, so we're adding that offset
                    // here to avoid rendering the glyph too high
                    bearing_y: if cfg!(target_os = "macos") { descender } else { 0. }
                        + (f64::from(ft_glyph.bitmap_top) * (dest_height as f64 / height as f64)),
                }
            }
            ftwrap::FT_Pixel_Mode::FT_PIXEL_MODE_GRAY => {
                let width = ft_glyph.bitmap.width as usize;
                let height = ft_glyph.bitmap.rows as usize;
                let size = (width * height * 4) as usize;
                let mut rgba = vec![0u8; size];
                for y in 0..height {
                    let src_offset = y * pitch;
                    let dest_offset = y * width * 4;
                    for x in 0..width {
                        let gray = data[src_offset + x];

                        rgba[dest_offset + (x * 4)] = gray;
                        rgba[dest_offset + (x * 4) + 1] = gray;
                        rgba[dest_offset + (x * 4) + 2] = gray;
                        rgba[dest_offset + (x * 4) + 3] = gray;
                    }
                }
                RasterizedGlyph {
                    data: rgba,
                    height,
                    width,
                    bearing_x: ft_glyph.bitmap_left as f64,
                    bearing_y: ft_glyph.bitmap_top as f64,
                }
            }
            ftwrap::FT_Pixel_Mode::FT_PIXEL_MODE_MONO => {
                let width = ft_glyph.bitmap.width as usize;
                let height = ft_glyph.bitmap.rows as usize;
                let size = (width * height * 4) as usize;
                let mut rgba = vec![0u8; size];
                for y in 0..height {
                    let src_offset = y * pitch;
                    let dest_offset = y * width * 4;
                    let mut x = 0;
                    for i in 0..pitch {
                        if x >= width {
                            break;
                        }
                        let mut b = data[src_offset + i];
                        for _ in 0..8 {
                            if x >= width {
                                break;
                            }
                            if b & 0x80 == 0x80 {
                                for j in 0..4 {
                                    rgba[dest_offset + (x * 4) + j] = 0xff;
                                }
                            }
                            b <<= 1;
                            x += 1;
                        }
                    }
                }
                RasterizedGlyph {
                    data: rgba,
                    height,
                    width,
                    bearing_x: ft_glyph.bitmap_left as f64,
                    bearing_y: ft_glyph.bitmap_top as f64,
                }
            }
            mode => bail!("unhandled pixel mode: {:?}", mode),
        };
        Ok(glyph)
    }
}
