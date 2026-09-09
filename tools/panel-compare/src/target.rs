//! A `DrawTarget` over an in-memory RGB image.
//!
//! Thirty lines instead of pulling in `embedded-graphics-simulator` and SDL:
//! the deliverable here is a printable sheet, not a window, and fewer moving
//! parts matter more than a live preview. SDL2 is installed on this machine if
//! on-screen iteration is ever wanted.

use embedded_graphics::pixelcolor::Rgb888;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::Rectangle;
use image::{Rgb, RgbImage};

pub struct ImageTarget {
    pub img: RgbImage,
}

impl ImageTarget {
    pub fn new(w: u32, h: u32, fill: Rgb888) -> Self {
        let mut img = RgbImage::new(w, h);
        for p in img.pixels_mut() {
            *p = Rgb([fill.r(), fill.g(), fill.b()]);
        }
        ImageTarget { img }
    }
}

impl OriginDimensions for ImageTarget {
    fn size(&self) -> Size {
        Size::new(self.img.width(), self.img.height())
    }
}

impl DrawTarget for ImageTarget {
    type Color = Rgb888;
    type Error = core::convert::Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        let (w, h) = (self.img.width() as i32, self.img.height() as i32);
        for Pixel(p, c) in pixels {
            if p.x >= 0 && p.y >= 0 && p.x < w && p.y < h {
                self.img
                    .put_pixel(p.x as u32, p.y as u32, Rgb([c.r(), c.g(), c.b()]));
            }
        }
        Ok(())
    }

    fn fill_solid(&mut self, area: &Rectangle, color: Self::Color) -> Result<(), Self::Error> {
        let c = Rgb([color.r(), color.g(), color.b()]);
        let (w, h) = (self.img.width() as i32, self.img.height() as i32);
        for y in area.top_left.y..area.top_left.y + area.size.height as i32 {
            for x in area.top_left.x..area.top_left.x + area.size.width as i32 {
                if x >= 0 && y >= 0 && x < w && y < h {
                    self.img.put_pixel(x as u32, y as u32, c);
                }
            }
        }
        Ok(())
    }
}
