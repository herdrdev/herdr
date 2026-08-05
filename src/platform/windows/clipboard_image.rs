use std::io::{self, Cursor, Write as _};

use crate::protocol::MAX_CLIPBOARD_IMAGE_PAYLOAD;

pub(super) const MAX_CLIPBOARD_ALLOCATION: usize = 64 * 1024 * 1024;

const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
const MAX_IMAGE_DIMENSION: u32 = 16_384;
const MAX_IMAGE_PIXELS: usize = 16 * 1024 * 1024;
const BI_RGB: u32 = 0;
const BI_BITFIELDS: u32 = 3;
const BI_ALPHABITFIELDS: u32 = 6;

pub(super) fn validated_png(bytes: &[u8]) -> Option<Vec<u8>> {
    let logical_len = png_logical_len(bytes)?;
    if logical_len > MAX_CLIPBOARD_IMAGE_PAYLOAD {
        return None;
    }
    let bytes = &bytes[..logical_len];
    let mut decoder = png::Decoder::new_with_limits(
        Cursor::new(bytes),
        png::Limits {
            bytes: MAX_CLIPBOARD_IMAGE_PAYLOAD,
        },
    );
    decoder.set_ignore_text_chunk(true);
    decoder.set_ignore_iccp_chunk(true);
    let mut reader = decoder.read_info().ok()?;
    validate_dimensions(reader.info().width, reader.info().height)?;
    if reader.info().animation_control.is_some() {
        return None;
    }
    while reader.next_interlaced_row().ok()?.is_some() {}
    reader.finish().ok()?;
    Some(bytes.to_vec())
}

fn png_logical_len(bytes: &[u8]) -> Option<usize> {
    if !bytes.starts_with(PNG_SIGNATURE) {
        return None;
    }
    let mut offset = PNG_SIGNATURE.len();
    while offset < bytes.len() {
        let length = usize::try_from(read_u32_be(bytes, offset)?).ok()?;
        let kind_start = offset.checked_add(4)?;
        let data_start = kind_start.checked_add(4)?;
        let data_end = data_start.checked_add(length)?;
        let chunk_end = data_end.checked_add(4)?;
        if chunk_end > bytes.len() {
            return None;
        }
        if bytes.get(kind_start..data_start)? == b"IEND" {
            return (length == 0).then_some(chunk_end);
        }
        offset = chunk_end;
    }
    None
}

pub(super) fn dib_to_png(bytes: &[u8]) -> Option<Vec<u8>> {
    Dib::parse(bytes)?.encode_png()
}

struct Dib<'a> {
    width: u32,
    height: u32,
    top_down: bool,
    stride: usize,
    pixels: &'a [u8],
    format: PixelFormat,
}

#[derive(Clone, Copy)]
enum PixelFormat {
    Bgr24,
    Bgrx32,
    Masked32 {
        red: ChannelMask,
        green: ChannelMask,
        blue: ChannelMask,
        alpha: Option<ChannelMask>,
    },
}

#[derive(Clone, Copy)]
struct ChannelMask {
    mask: u32,
    shift: u32,
    maximum: u32,
}

impl ChannelMask {
    fn parse(mask: u32) -> Option<Self> {
        if mask == 0 {
            return None;
        }
        let shift = mask.trailing_zeros();
        let maximum = mask >> shift;
        if maximum & maximum.wrapping_add(1) != 0 {
            return None;
        }
        Some(Self {
            mask,
            shift,
            maximum,
        })
    }

    fn extract(self, pixel: u32) -> u8 {
        let value = (pixel & self.mask) >> self.shift;
        ((u64::from(value) * 255 + u64::from(self.maximum) / 2) / u64::from(self.maximum)) as u8
    }
}

impl<'a> Dib<'a> {
    fn parse(bytes: &'a [u8]) -> Option<Self> {
        let header_size = usize::try_from(read_u32_le(bytes, 0)?).ok()?;
        if !matches!(header_size, 40 | 52 | 56 | 108 | 124) || bytes.len() < header_size {
            return None;
        }
        let signed_width = read_i32_le(bytes, 4)?;
        let signed_height = read_i32_le(bytes, 8)?;
        if signed_width <= 0 || signed_height == 0 || signed_height == i32::MIN {
            return None;
        }
        let width = u32::try_from(signed_width).ok()?;
        let height = signed_height.unsigned_abs();
        validate_dimensions(width, height)?;
        if read_u16_le(bytes, 12)? != 1 {
            return None;
        }

        let bit_count = read_u16_le(bytes, 14)?;
        let compression = read_u32_le(bytes, 16)?;
        let colors_used = usize::try_from(read_u32_le(bytes, 32)?).ok()?;
        let (format, external_mask_bytes) =
            pixel_format(bytes, header_size, bit_count, compression)?;
        let palette_bytes = colors_used.checked_mul(4)?;
        let pixel_offset = header_size
            .checked_add(external_mask_bytes)?
            .checked_add(palette_bytes)?;
        let row_bits = usize::try_from(width)
            .ok()?
            .checked_mul(usize::from(bit_count))?;
        let stride = row_bits.checked_add(31)?.checked_div(32)?.checked_mul(4)?;
        let image_size = stride.checked_mul(usize::try_from(height).ok()?)?;
        let pixels = bytes.get(pixel_offset..pixel_offset.checked_add(image_size)?)?;

        Some(Self {
            width,
            height,
            top_down: signed_height < 0,
            stride,
            pixels,
            format,
        })
    }

    fn encode_png(&self) -> Option<Vec<u8>> {
        let mut output = LimitedWriter::new(MAX_CLIPBOARD_IMAGE_PAYLOAD);
        {
            let mut encoder = png::Encoder::new(&mut output, self.width, self.height);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().ok()?;
            let mut stream = writer.stream_writer().ok()?;
            let mut rgba = vec![0_u8; usize::try_from(self.width).ok()?.checked_mul(4)?];
            for output_row in 0..usize::try_from(self.height).ok()? {
                let source_row = if self.top_down {
                    output_row
                } else {
                    usize::try_from(self.height)
                        .ok()?
                        .checked_sub(output_row + 1)?
                };
                let start = source_row.checked_mul(self.stride)?;
                let source = self.pixels.get(start..start.checked_add(self.stride)?)?;
                self.decode_row(source, &mut rgba)?;
                stream.write_all(&rgba).ok()?;
            }
            stream.finish().ok()?;
            writer.finish().ok()?;
        }
        Some(output.into_inner())
    }

    fn decode_row(&self, source: &[u8], output: &mut [u8]) -> Option<()> {
        match self.format {
            PixelFormat::Bgr24 => {
                for (pixel, rgba) in source
                    .chunks_exact(3)
                    .take(usize::try_from(self.width).ok()?)
                    .zip(output.chunks_exact_mut(4))
                {
                    rgba.copy_from_slice(&[pixel[2], pixel[1], pixel[0], 255]);
                }
            }
            PixelFormat::Bgrx32 => {
                for (pixel, rgba) in source
                    .chunks_exact(4)
                    .take(usize::try_from(self.width).ok()?)
                    .zip(output.chunks_exact_mut(4))
                {
                    rgba.copy_from_slice(&[pixel[2], pixel[1], pixel[0], 255]);
                }
            }
            PixelFormat::Masked32 {
                red,
                green,
                blue,
                alpha,
            } => {
                for (pixel, rgba) in source
                    .chunks_exact(4)
                    .take(usize::try_from(self.width).ok()?)
                    .zip(output.chunks_exact_mut(4))
                {
                    let pixel = u32::from_le_bytes(pixel.try_into().ok()?);
                    rgba.copy_from_slice(&[
                        red.extract(pixel),
                        green.extract(pixel),
                        blue.extract(pixel),
                        alpha.map_or(255, |mask| mask.extract(pixel)),
                    ]);
                }
            }
        }
        Some(())
    }
}

fn pixel_format(
    bytes: &[u8],
    header_size: usize,
    bit_count: u16,
    compression: u32,
) -> Option<(PixelFormat, usize)> {
    match (bit_count, compression) {
        (24, BI_RGB) => return Some((PixelFormat::Bgr24, 0)),
        (32, BI_RGB) => return Some((PixelFormat::Bgrx32, 0)),
        (32, BI_BITFIELDS | BI_ALPHABITFIELDS) => {}
        _ => return None,
    }

    let (offset, external_mask_bytes) = if header_size == 40 {
        let count = if compression == BI_ALPHABITFIELDS {
            4
        } else {
            3
        };
        (40, count * 4)
    } else {
        if header_size < 52 || (compression == BI_ALPHABITFIELDS && header_size < 56) {
            return None;
        }
        (40, 0)
    };
    let red = ChannelMask::parse(read_u32_le(bytes, offset)?)?;
    let green = ChannelMask::parse(read_u32_le(bytes, offset + 4)?)?;
    let blue = ChannelMask::parse(read_u32_le(bytes, offset + 8)?)?;
    if red.mask & green.mask != 0 || red.mask & blue.mask != 0 || green.mask & blue.mask != 0 {
        return None;
    }
    let alpha = if compression == BI_ALPHABITFIELDS || header_size >= 56 {
        match read_u32_le(bytes, offset + 12).filter(|mask| *mask != 0) {
            Some(mask) => Some(ChannelMask::parse(mask)?),
            None => None,
        }
    } else {
        None
    };
    if alpha.is_some_and(|alpha| alpha.mask & (red.mask | green.mask | blue.mask) != 0) {
        return None;
    }
    Some((
        PixelFormat::Masked32 {
            red,
            green,
            blue,
            alpha,
        },
        external_mask_bytes,
    ))
}

fn validate_dimensions(width: u32, height: u32) -> Option<()> {
    if width == 0 || height == 0 || width > MAX_IMAGE_DIMENSION || height > MAX_IMAGE_DIMENSION {
        return None;
    }
    let pixels = usize::try_from(width)
        .ok()?
        .checked_mul(usize::try_from(height).ok()?)?;
    (pixels <= MAX_IMAGE_PIXELS).then_some(())
}

struct LimitedWriter {
    bytes: Vec<u8>,
    limit: usize,
}

impl LimitedWriter {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
        }
    }

    fn into_inner(self) -> Vec<u8> {
        self.bytes
    }
}

impl io::Write for LimitedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                "encoded clipboard image exceeds protocol limit",
            ));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn read_u16_le(bytes: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        bytes.get(offset..offset + 2)?.try_into().ok()?,
    ))
}

fn read_u32_le(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        bytes.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

fn read_i32_le(bytes: &[u8], offset: usize) -> Option<i32> {
    Some(i32::from_le_bytes(
        bytes.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

fn read_u32_be(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_be_bytes(
        bytes.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info_header(width: i32, height: i32, bit_count: u16, compression: u32) -> Vec<u8> {
        let mut header = vec![0_u8; 40];
        header[0..4].copy_from_slice(&40_u32.to_le_bytes());
        header[4..8].copy_from_slice(&width.to_le_bytes());
        header[8..12].copy_from_slice(&height.to_le_bytes());
        header[12..14].copy_from_slice(&1_u16.to_le_bytes());
        header[14..16].copy_from_slice(&bit_count.to_le_bytes());
        header[16..20].copy_from_slice(&compression.to_le_bytes());
        header
    }

    fn decode_rgba(bytes: &[u8]) -> Vec<u8> {
        let mut decoder = png::Decoder::new(Cursor::new(bytes));
        decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
        let mut reader = decoder.read_info().unwrap();
        let mut output = vec![0_u8; reader.output_buffer_size()];
        let info = reader.next_frame(&mut output).unwrap();
        output.truncate(info.buffer_size());
        output
    }

    #[test]
    fn converts_bottom_up_24_bit_dib_to_png() {
        let mut dib = info_header(2, 2, 24, BI_RGB);
        dib.extend_from_slice(&[
            255, 0, 0, 255, 255, 255, 0, 0, // bottom: blue, white, padding
            0, 0, 255, 0, 255, 0, 0, 0, // top: red, green, padding
        ]);

        let png = dib_to_png(&dib).unwrap();
        assert_eq!(
            decode_rgba(&png),
            vec![255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,]
        );
    }

    #[test]
    fn strips_registered_png_allocator_tail() {
        let mut bytes = Vec::new();
        let mut encoder = png::Encoder::new(&mut bytes, 1, 1);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&[1, 2, 3, 255]).unwrap();
        writer.finish().unwrap();
        let expected = bytes.clone();
        bytes.extend_from_slice(&[0; 32]);

        assert_eq!(validated_png(&bytes), Some(expected));
    }
}
