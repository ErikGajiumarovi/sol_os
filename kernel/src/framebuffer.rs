use bootloader_api::info::{FrameBuffer, FrameBufferInfo, PixelFormat};
use core::fmt::{self, Write};
use noto_sans_mono_bitmap::{FontWeight, RasterHeight, get_raster, get_raster_width};
use spin::Mutex;

const FONT_WEIGHT: FontWeight = FontWeight::Regular;
const FONT_HEIGHT: RasterHeight = RasterHeight::Size16;
const LETTER_SPACING: usize = 1;
const LINE_SPACING: usize = 2;
const BORDER: usize = 8;
const FOREGROUND: [u8; 3] = [0xe6, 0xed, 0xf3];
const BACKGROUND: [u8; 3] = [0x0a, 0x10, 0x20];

static WRITER: Mutex<Option<FramebufferWriter>> = Mutex::new(None);

pub fn init(framebuffer: FrameBuffer) {
    let info = framebuffer.info();
    let buffer = framebuffer.into_buffer();
    *WRITER.lock() = Some(FramebufferWriter::new(buffer, info));
}

pub fn clear() {
    if let Some(writer) = WRITER.lock().as_mut() {
        writer.clear();
    }
}

pub fn print(args: fmt::Arguments<'_>) {
    if let Some(writer) = WRITER.lock().as_mut() {
        let _ = writer.write_fmt(args);
    }
}

pub fn panic_print(args: fmt::Arguments<'_>) {
    if let Some(mut guard) = WRITER.try_lock() {
        if let Some(writer) = guard.as_mut() {
            let _ = writer.write_fmt(args);
        }
    }
}

struct FramebufferWriter {
    buffer: &'static mut [u8],
    info: FrameBufferInfo,
    column: usize,
    row: usize,
}

impl FramebufferWriter {
    fn new(buffer: &'static mut [u8], info: FrameBufferInfo) -> Self {
        Self {
            buffer,
            info,
            column: BORDER,
            row: BORDER,
        }
    }

    fn clear(&mut self) {
        for y in 0..self.info.height {
            for x in 0..self.info.width {
                self.write_pixel(x, y, BACKGROUND);
            }
        }
        self.column = BORDER;
        self.row = BORDER;
    }

    fn write_character(&mut self, character: char) {
        if character == '\n' {
            self.new_line();
            return;
        }

        let raster = get_raster(character, FONT_WEIGHT, FONT_HEIGHT)
            .or_else(|| get_raster('?', FONT_WEIGHT, FONT_HEIGHT))
            .expect("fallback glyph is missing");
        let advance = raster.width() + LETTER_SPACING;
        if self.column + advance + BORDER > self.info.width {
            self.new_line();
        }

        for (y, row) in raster.raster().iter().enumerate() {
            for (x, intensity) in row.iter().copied().enumerate() {
                let color = blend(BACKGROUND, FOREGROUND, intensity);
                self.write_pixel(self.column + x, self.row + y, color);
            }
        }
        self.column += advance;
    }

    fn new_line(&mut self) {
        self.column = BORDER;
        self.row += FONT_HEIGHT.val() + LINE_SPACING;
        if self.row + FONT_HEIGHT.val() + BORDER > self.info.height {
            self.scroll();
        }
    }

    fn scroll(&mut self) {
        let pixel_rows = FONT_HEIGHT.val() + LINE_SPACING;
        let byte_rows = pixel_rows * self.info.stride * self.info.bytes_per_pixel;
        self.buffer.copy_within(byte_rows.., 0);

        let first_clear_row = self.info.height.saturating_sub(pixel_rows);
        for y in first_clear_row..self.info.height {
            for x in 0..self.info.width {
                self.write_pixel(x, y, BACKGROUND);
            }
        }
        self.row = self.row.saturating_sub(pixel_rows);
    }

    fn write_pixel(&mut self, x: usize, y: usize, rgb: [u8; 3]) {
        if x >= self.info.width || y >= self.info.height {
            return;
        }
        let offset = (y * self.info.stride + x) * self.info.bytes_per_pixel;
        let color = match self.info.pixel_format {
            PixelFormat::Rgb => [rgb[0], rgb[1], rgb[2]],
            PixelFormat::Bgr => [rgb[2], rgb[1], rgb[0]],
            PixelFormat::U8 => {
                let gray = ((u16::from(rgb[0]) + u16::from(rgb[1]) + u16::from(rgb[2])) / 3) as u8;
                [gray, gray, gray]
            }
            _ => [rgb[2], rgb[1], rgb[0]],
        };
        let count = self.info.bytes_per_pixel.min(3);
        self.buffer[offset..offset + count].copy_from_slice(&color[..count]);
        if self.info.bytes_per_pixel > 3 {
            self.buffer[offset + 3] = 0;
        }
    }
}

impl Write for FramebufferWriter {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        for character in text.chars() {
            match character {
                '\r' => self.column = BORDER,
                '\t' => {
                    for _ in 0..4 {
                        self.write_character(' ');
                    }
                }
                character if character.is_ascii() => self.write_character(character),
                _ => self.write_character('?'),
            }
        }
        Ok(())
    }
}

fn blend(background: [u8; 3], foreground: [u8; 3], intensity: u8) -> [u8; 3] {
    let alpha = u16::from(intensity);
    let inverse = 255 - alpha;
    [
        ((u16::from(background[0]) * inverse + u16::from(foreground[0]) * alpha) / 255) as u8,
        ((u16::from(background[1]) * inverse + u16::from(foreground[1]) * alpha) / 255) as u8,
        ((u16::from(background[2]) * inverse + u16::from(foreground[2]) * alpha) / 255) as u8,
    ]
}

#[allow(dead_code)]
const CHARACTER_WIDTH: usize = get_raster_width(FONT_WEIGHT, FONT_HEIGHT) + LETTER_SPACING;
