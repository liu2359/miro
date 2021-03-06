//! Higher level freetype bindings

use failure::Error;
pub use freetype::freetype::*;
use std::ffi::CString;
use std::ptr;

/// Translate an error and value into a result
fn ft_result<T>(err: FT_Error, t: T) -> Result<T, Error> {
    if err.succeeded() {
        Ok(t)
    } else {
        Err(format_err!("FreeType error {:?}", err))
    }
}

pub struct Face {
    pub face: FT_Face,
}

impl Drop for Face {
    fn drop(&mut self) {
        unsafe {
            FT_Done_Face(self.face);
        }
    }
}

impl Face {
    pub fn set_char_size(
        &mut self,
        char_width: FT_F26Dot6,
        char_height: FT_F26Dot6,
        horz_resolution: FT_UInt,
        vert_resolution: FT_UInt,
    ) -> Result<(), Error> {
        ft_result(
            unsafe {
                FT_Set_Char_Size(
                    self.face,
                    char_width,
                    char_height,
                    horz_resolution,
                    vert_resolution,
                )
            },
            (),
        )
    }

    #[allow(unused)]
    pub fn set_pixel_sizes(&mut self, char_width: u32, char_height: u32) -> Result<(), Error> {
        ft_result(unsafe { FT_Set_Pixel_Sizes(self.face, char_width, char_height) }, ())
    }

    pub fn select_size(&mut self, idx: usize) -> Result<(), Error> {
        ft_result(unsafe { FT_Select_Size(self.face, idx as i32) }, ())
    }

    pub fn load_codepoint(
        &mut self,
        codepoint: char,
    ) -> Result<(FT_UInt, FT_Glyph_Metrics_), Error> {
        unsafe {
            let glyph_pos = FT_Get_Char_Index(self.face, codepoint as u32 as _);
            let res = FT_Load_Glyph(self.face, glyph_pos, FT_LOAD_COLOR as i32);
            ensure!(res.succeeded(), "load_codepoint {}: FreeType error {:?}", codepoint, res);

            let glyph = &(*(*self.face).glyph);
            Ok((glyph_pos, glyph.metrics))
        }
    }

    pub fn load_and_render_glyph(
        &mut self,
        glyph_index: FT_UInt,
        load_flags: FT_Int32,
        render_mode: FT_Render_Mode,
    ) -> Result<&FT_GlyphSlotRec_, Error> {
        unsafe {
            let res = FT_Load_Glyph(self.face, glyph_index, load_flags);
            if res.succeeded() {
                let render = FT_Render_Glyph((*self.face).glyph, render_mode);
                if !render.succeeded() {
                    bail!("FT_Render_Glyph failed: {:?}", render);
                }
            }
            ft_result(res, &*(*self.face).glyph)
        }
    }

    pub fn cell_metrics(&mut self) -> (f64, f64) {
        unsafe {
            let metrics = &(*(*self.face).size).metrics;
            let height = (metrics.y_scale as f64 * f64::from((*self.face).height))
                / (f64::from(0x1_0000) * 64.0);

            let mut width = 0.0;
            for i in 32..128 {
                let glyph_pos = FT_Get_Char_Index(self.face, i);
                let res = FT_Load_Glyph(self.face, glyph_pos, FT_LOAD_COLOR as i32);
                if res.succeeded() {
                    let glyph = &(*(*self.face).glyph);
                    if glyph.metrics.horiAdvance as f64 > width {
                        width = glyph.metrics.horiAdvance as f64;
                    }
                }
            }
            (width / 64.0, height)
        }
    }
}

pub struct Library {
    lib: FT_Library,
}

impl Drop for Library {
    fn drop(&mut self) {
        unsafe {
            FT_Done_FreeType(self.lib);
        }
    }
}

impl Library {
    pub fn new() -> Result<Library, Error> {
        let mut lib = ptr::null_mut();
        let res = unsafe { FT_Init_FreeType(&mut lib as *mut _) };
        let lib = ft_result(res, lib)?;
        Ok(Library { lib })
    }

    #[allow(dead_code)]
    pub fn new_face<P>(&self, path: P, face_index: FT_Long) -> Result<Face, Error>
    where
        P: Into<Vec<u8>>,
    {
        let mut face = ptr::null_mut();
        let path = CString::new(path.into())?;

        let res = unsafe { FT_New_Face(self.lib, path.as_ptr(), face_index, &mut face as *mut _) };
        Ok(Face { face: ft_result(res, face)? })
    }

    pub fn new_face_from_slice(&self, data: &[u8], face_index: FT_Long) -> Result<Face, Error> {
        let mut face = ptr::null_mut();

        let res = unsafe {
            FT_New_Memory_Face(
                self.lib,
                data.as_ptr(),
                data.len() as _,
                face_index,
                &mut face as *mut _,
            )
        };
        Ok(Face { face: ft_result(res, face)? })
    }

    pub fn set_lcd_filter(&mut self, filter: FT_LcdFilter) -> Result<(), Error> {
        unsafe { ft_result(FT_Library_SetLcdFilter(self.lib, filter), ()) }
    }
}
