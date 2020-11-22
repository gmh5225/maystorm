// Font Driver

use crate::io::graphics::*;
use crate::*;
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::vec::*;

// include!("megbtan.rs");
include!("megh0816.rs");
const SYSTEM_FONT: FixedFontDriver = FixedFontDriver::new(8, 16, &FONT_MEGH0816_DATA);

include!("megh0608.rs");
const SMALL_FONT: FixedFontDriver = FixedFontDriver::new(6, 8, &FONT_MEGH0608_DATA);

static mut FONT_MANAGER: FontManager = FontManager::new();

pub struct FontManager {
    fonts: Option<BTreeMap<FontFamily, Box<dyn FontDriver>>>,
    lock: Spinlock,
    buffer: OperationalBitmapResticted,
}

impl FontManager {
    const fn new() -> Self {
        Self {
            fonts: None,
            lock: Spinlock::new(),
            buffer: OperationalBitmapResticted::new(96, 96),
        }
    }

    #[inline]
    fn shared<'a>() -> &'a mut Self {
        unsafe { &mut FONT_MANAGER }
    }

    pub(crate) fn init() {
        let shared = Self::shared();

        let mut fonts: BTreeMap<FontFamily, Box<dyn FontDriver>> = BTreeMap::new();

        fonts.insert(FontFamily::FixedSystem, Box::new(SYSTEM_FONT));
        fonts.insert(FontFamily::SmallFixed, Box::new(SMALL_FONT));

        let font = Box::new(HersheyFont::new(
            0,
            include_bytes!("../../../../../ext/hershey/futural.jhf"),
        ));
        fonts.insert(FontFamily::SystemUI, font);

        let font = Box::new(HersheyFont::new(
            4,
            include_bytes!("../../../../../ext/hershey/cursive.jhf"),
        ));
        fonts.insert(FontFamily::Cursive, font);

        let font = Box::new(HersheyFont::new(
            0,
            include_bytes!("../../../../../ext/hershey/futuram.jhf"),
        ));
        fonts.insert(FontFamily::SansSerif, font);

        let font = Box::new(HersheyFont::new(
            0,
            include_bytes!("../../../../../ext/hershey/timesr.jhf"),
        ));
        fonts.insert(FontFamily::Serif, font);

        shared.fonts = Some(fonts);
    }

    fn driver_for(family: FontFamily) -> Option<&'static dyn FontDriver> {
        let shared = Self::shared();
        shared
            .fonts
            .as_ref()
            .and_then(|v| v.get(&family))
            .map(|v| v.as_ref())
    }

    #[inline]
    pub const fn fixed_system_font() -> &'static FixedFontDriver<'static> {
        &SYSTEM_FONT
    }

    #[inline]
    pub fn system_font() -> FontDescriptor {
        FontDescriptor::new(FontFamily::FixedSystem, 0).unwrap()
    }

    #[inline]
    pub fn title_font() -> FontDescriptor {
        FontDescriptor::new(FontFamily::SansSerif, 16).unwrap_or(Self::system_font())
    }

    #[inline]
    pub fn label_font() -> FontDescriptor {
        FontDescriptor::new(FontFamily::SystemUI, 16).unwrap_or(Self::system_font())
    }
}

#[non_exhaustive]
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum FontFamily {
    SystemUI,
    SansSerif,
    Serif,
    Cursive,
    FixedSystem,
    SmallFixed,
    Japanese,
}

#[derive(Copy, Clone)]
pub struct FontDescriptor {
    driver: &'static dyn FontDriver,
    point: i32,
    line_height: i32,
}

impl FontDescriptor {
    pub fn new(family: FontFamily, point: isize) -> Option<Self> {
        FontManager::driver_for(family).map(|driver| {
            if driver.is_scalable() {
                Self {
                    driver,
                    point: point as i32,
                    line_height: (driver.preferred_line_height() * point / driver.base_height())
                        as i32,
                }
            } else {
                Self {
                    driver,
                    point: driver.base_height() as i32,
                    line_height: driver.preferred_line_height() as i32,
                }
            }
        })
    }

    #[inline]
    pub const fn point(&self) -> isize {
        self.point as isize
    }

    #[inline]
    pub const fn line_height(&self) -> isize {
        self.line_height as isize
    }

    #[inline]
    pub fn width_of(&self, character: char) -> isize {
        if self.point() == self.driver.base_height() {
            self.driver.width_of(character)
        } else {
            self.driver.width_of(character) * self.point() / self.driver.base_height()
        }
    }

    #[inline]
    pub fn is_scalable(&self) -> bool {
        self.driver.is_scalable()
    }

    #[inline]
    pub fn draw_char(&self, character: char, bitmap: &Bitmap, origin: Point<isize>, color: Color) {
        self.driver
            .draw_char(character, bitmap, origin, self.point(), color)
    }
}

pub trait FontDriver {
    fn is_scalable(&self) -> bool;

    fn base_height(&self) -> isize;

    fn preferred_line_height(&self) -> isize;

    fn width_of(&self, character: char) -> isize;

    fn draw_char(
        &self,
        character: char,
        bitmap: &Bitmap,
        origin: Point<isize>,
        height: isize,
        color: Color,
    );
}

pub struct FixedFontDriver<'a> {
    size: Size<isize>,
    data: &'a [u8],
    leading: isize,
    line_height: isize,
    stride: usize,
}

impl FixedFontDriver<'_> {
    pub const fn new(width: usize, height: usize, data: &'static [u8]) -> FixedFontDriver<'static> {
        let width = width as isize;
        let height = height as isize;
        let line_height = height * 5 / 4;
        let leading = (line_height - height) / 2;
        let stride = ((width as usize + 7) >> 3) * height as usize;
        FixedFontDriver {
            size: Size::new(width, height),
            line_height,
            leading,
            stride,
            data,
        }
    }

    #[inline]
    pub const fn width(&self) -> isize {
        self.size.width
    }

    #[inline]
    pub const fn line_height(&self) -> isize {
        self.line_height
    }

    /// Glyph Data for Rasterized Font
    fn glyph_for(&self, character: char) -> Option<&[u8]> {
        let c = character as usize;
        if c > 0x20 && c < 0x80 {
            let base = self.stride * (c - 0x20);
            Some(&self.data[base..base + self.stride])
        } else {
            None
        }
    }
}

impl FontDriver for FixedFontDriver<'_> {
    #[inline]
    fn is_scalable(&self) -> bool {
        false
    }

    #[inline]
    fn base_height(&self) -> isize {
        self.size.height
    }

    #[inline]
    fn preferred_line_height(&self) -> isize {
        self.line_height
    }

    #[inline]
    fn width_of(&self, character: char) -> isize {
        let _ = character;
        self.size.width
    }

    fn draw_char(
        &self,
        character: char,
        bitmap: &Bitmap,
        origin: Point<isize>,
        height: isize,
        color: Color,
    ) {
        let _ = height;
        if let Some(glyph) = self.glyph_for(character) {
            let rect = Rect::new(
                origin.x,
                origin.y + self.leading,
                self.width(),
                self.base_height(),
            );
            bitmap.draw_pattern(rect, glyph, color);
        }
    }
}

#[allow(dead_code)]
struct HersheyFont<'a> {
    data: &'a [u8],
    line_height: isize,
    glyph_info: Vec<(usize, usize, isize)>,
}

impl<'a> HersheyFont<'a> {
    const MAGIC_20: isize = 0x20;
    const MAGIC_52: isize = 0x52;
    const POINT: isize = 32;
    const DESCENT: isize = 2;

    fn new(extra_height: isize, font_data: &'a [u8]) -> Self {
        let descent = Self::DESCENT + extra_height;
        let mut font = Self {
            data: font_data,
            line_height: Self::POINT + descent,
            glyph_info: Vec::with_capacity(96),
        };

        for c in 0x20..0x80 {
            let character = c as u8 as char;

            let (base, last) = match font.search_for_glyph(character) {
                Some(tuple) => tuple,
                None => break,
            };

            let data = &font_data[base..last];

            let w1 = data[8] as isize;
            let w2 = data[9] as isize;

            font.glyph_info.push((base, last, w2 - w1));
        }

        font
    }

    fn draw_data(
        &self,
        data: &[u8],
        bitmap: &Bitmap,
        origin: Point<isize>,
        width: isize,
        height: isize,
        color: Color,
    ) {
        let _ = width;
        if data.len() >= 12 {
            FontManager::shared().lock.synchronized(|| {
                let shared = FontManager::shared();
                shared.buffer.reset();

                let n_pairs = (data[6] & 0x0F) * 10 + (data[7] & 0x0F);
                let left = data[8] as isize - Self::MAGIC_52;

                let center = Point::new(
                    shared.buffer.width() / 2 - 1,
                    shared.buffer.height() / 2 - 1,
                );
                let mut cursor = 10;
                let mut c0: Option<Point<isize>> = None;
                for _ in 1..n_pairs {
                    let c1 = data[cursor] as isize;
                    let c2 = data[cursor + 1] as isize;
                    if c1 == Self::MAGIC_20 && c2 == Self::MAGIC_52 {
                        c0 = None;
                    } else {
                        let d1 = c1 - Self::MAGIC_52;
                        let d2 = c2 - Self::MAGIC_52;
                        let c1 = center
                            + Point::new(
                                d1 * 2 * height / Self::POINT,
                                d2 * 2 * height / Self::POINT,
                            );
                        if let Some(c0) = c0 {
                            shared.buffer.draw_line(c0, c1, |bitmap, point| {
                                bitmap.restrict_mut(point, EdgeInsets::padding_each(1), |bitmap| {
                                    if bitmap.get_pixel(point) != u8::MAX {
                                        let level1 = 120;

                                        bitmap.process_pixel(point + Point::new(0, -1), |v| {
                                            v.saturating_add(level1)
                                        });

                                        let line = bitmap.fetch_line(point.y);
                                        line[point.x as usize - 1] =
                                            line[point.x as usize - 1].saturating_add(level1);
                                        line[point.x as usize] = u8::MAX;
                                        line[point.x as usize + 1] =
                                            line[point.x as usize + 1].saturating_add(level1);

                                        bitmap.process_pixel(point + Point::new(0, 1), |v| {
                                            v.saturating_add(level1)
                                        });
                                    }
                                });
                            });
                        }
                        c0 = Some(c1);
                    }
                    cursor += 2;
                }

                // DEBUG
                if false {
                    let rect = Rect::new(
                        origin.x,
                        origin.y,
                        width * height / Self::POINT,
                        self.line_height * height / Self::POINT,
                    );
                    bitmap.draw_rect(rect, Color::from_rgb(0xFFCCFF));
                    bitmap.draw_hline(
                        Point::new(origin.x, origin.y + height - 1),
                        width * height / Self::POINT,
                        Color::from_rgb(0xFFFF33),
                    );
                    bitmap.draw_hline(
                        Point::new(origin.x, origin.y + height * 3 / 4),
                        width * height / Self::POINT,
                        Color::from_rgb(0xFF3333),
                    );
                }

                let shared = FontManager::shared();
                let offset_x = (shared.buffer.width() / 4) + left * height / self.base_height() - 1;
                let offset_y = (shared.buffer.height() / 2 - height) / 2;
                shared.buffer.transform(
                    origin - Point::new(offset_x, offset_y),
                    shared.buffer.size() / 2,
                    |point, alpha| {
                        if alpha > 0 {
                            let mut c = color.components();
                            c.a = alpha;
                            bitmap.blend_pixel(point, c.into());
                        }
                    },
                );
            })
        }
    }

    fn search_for_glyph(&self, character: char) -> Option<(usize, usize)> {
        let c = character as usize;
        if c >= 0x20 && c < 0x80 {
            let c = c - 0x20;
            let mut cursor = 0;
            for current in 0..96 {
                if self.data.len() <= cursor {
                    return None;
                }
                if current == c {
                    let base = cursor;
                    while self.data[cursor] >= 0x20 {
                        cursor += 1;
                    }
                    return Some((base, cursor));
                }
                while self.data[cursor] >= 0x20 {
                    cursor += 1;
                }
                cursor += 1;
            }
        }
        None
    }

    fn glyph_for(&self, character: char) -> Option<(usize, usize, isize)> {
        let i = (character as usize) - 0x20;
        if i < (0x80 - 0x20) && i < self.glyph_info.len() {
            return Some(self.glyph_info[i]);
        }
        None
    }
}

impl FontDriver for HersheyFont<'_> {
    #[inline]
    fn is_scalable(&self) -> bool {
        true
    }

    #[inline]
    fn base_height(&self) -> isize {
        Self::POINT
    }

    #[inline]
    fn preferred_line_height(&self) -> isize {
        self.line_height
    }

    fn width_of(&self, character: char) -> isize {
        match self.glyph_for(character) {
            Some(info) => info.2,
            None => 0,
        }
    }

    fn draw_char(
        &self,
        character: char,
        bitmap: &Bitmap,
        origin: Point<isize>,
        point: isize,
        color: Color,
    ) {
        let (base, last, width) = match self.glyph_for(character) {
            Some(info) => info,
            None => return,
        };
        let data = &self.data[base..last];
        self.draw_data(data, &bitmap, origin, width, point, color);
    }
}
