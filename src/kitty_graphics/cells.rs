//! Half-block cell rendering of pane images for host terminals that cannot
//! show Kitty graphics. Pure pixel math; the client caches each result per
//! image and placement geometry so a frame only repaints cached cells.
use std::sync::Arc;

use crate::protocol::{color_to_u32, FrameData, SurfaceGraphicsFormat};

use super::{ClippedPlacement, HostCellSize};

/// Pixel samples per axis averaged into one half-cell, bounding the work for
/// large images displayed small.
const MAX_SAMPLES_PER_AXIS: u64 = 8;
/// Averaged alpha at or above this paints the half-cell; below it the cell's
/// existing background shows through.
const OPAQUE_ALPHA: u32 = 128;
/// Largest RGBA buffer a PNG asset may decode into, matching the off-screen
/// image budget in `surface`.
const MAX_DECODED_BYTES: u64 = 64 * 1024 * 1024;

/// Where a placement's visible cells sample its image.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub(crate) struct CellGeometry {
    pub(crate) cols: u32,
    pub(crate) rows: u32,
    pub(crate) source_x: u32,
    pub(crate) source_y: u32,
    pub(crate) source_width: u32,
    pub(crate) source_height: u32,
    pub(crate) x_offset: u32,
    pub(crate) y_offset: u32,
    pub(crate) cell_size: HostCellSize,
}

impl CellGeometry {
    pub(super) fn from_clipped(clipped: ClippedPlacement, cell_size: HostCellSize) -> Self {
        Self {
            cols: clipped.cols,
            rows: clipped.rows,
            source_x: clipped.source_x,
            source_y: clipped.source_y,
            source_width: clipped.source_width,
            source_height: clipped.source_height,
            x_offset: clipped.x_offset,
            y_offset: clipped.y_offset,
            cell_size,
        }
    }
}

/// 8-bit pixels of one image; RGB sources carry no alpha byte.
pub(crate) struct Pixels<'a> {
    data: &'a [u8],
    width: u32,
    height: u32,
    channels: usize,
}

impl<'a> Pixels<'a> {
    pub(crate) fn new(
        format: SurfaceGraphicsFormat,
        width: u32,
        height: u32,
        data: &'a [u8],
    ) -> Option<Self> {
        let channels = match format {
            SurfaceGraphicsFormat::Rgb => 3,
            SurfaceGraphicsFormat::Rgba => 4,
            SurfaceGraphicsFormat::Png => return None,
        };
        let expected = (width as usize)
            .checked_mul(height as usize)?
            .checked_mul(channels)?;
        (data.len() == expected && width > 0 && height > 0).then_some(Self {
            data,
            width,
            height,
            channels,
        })
    }

    fn rgba(&self, x: u32, y: u32) -> [u8; 4] {
        let index = (y as usize * self.width as usize + x as usize) * self.channels;
        let pixel = &self.data[index..index + self.channels];
        [pixel[0], pixel[1], pixel[2], *pixel.get(3).unwrap_or(&255)]
    }
}

/// Decodes a PNG asset into 8-bit RGBA with the dimensions its key declares.
pub(crate) fn decode_png(data: &[u8], width: u32, height: u32) -> Option<Arc<[u8]>> {
    let mut decoder = png::Decoder::new(data);
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().ok()?;
    let header = (reader.info().width, reader.info().height);
    if header != (width, height) || u64::from(width) * u64::from(height) * 4 > MAX_DECODED_BYTES {
        tracing::debug!(
            declared = ?(width, height),
            ?header,
            "png asset disagrees with its key or exceeds the decode budget"
        );
        return None;
    }
    let mut buffer = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buffer).ok()?;
    let decoded = &buffer[..info.buffer_size()];
    let rgba: Vec<u8> = match info.color_type {
        png::ColorType::Rgba => decoded.to_vec(),
        png::ColorType::Rgb => decoded
            .chunks_exact(3)
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        png::ColorType::Grayscale => decoded.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::GrayscaleAlpha => decoded
            .chunks_exact(2)
            .flat_map(|p| [p[0], p[0], p[0], p[1]])
            .collect(),
        png::ColorType::Indexed => return None,
    };
    Some(Arc::from(rgba))
}

/// Two vertically stacked colors per cell, row-major; `None` is transparent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CellImage {
    cols: u32,
    rows: u32,
    samples: Vec<Option<[u8; 3]>>,
}

/// Source pixels shown across the display span `pos`. The image starts
/// `offset` into its cells and is scaled across the remaining display span,
/// matching the Kitty placement geometry in `piece_source_span`. A span that
/// ends inside the offset shows nothing; an upscaled sliver still shows one
/// source pixel.
fn source_span(
    pos: (u64, u64),
    display: u64,
    offset: u64,
    source: (u64, u64),
) -> Option<(u64, u64)> {
    let offset = offset.min(display.saturating_sub(1));
    if pos.1 <= offset {
        return None;
    }
    let edge = |pos: u64| (pos.max(offset) - offset) * source.1 / (display - offset).max(1);
    let start = edge(pos.0);
    let end = edge(pos.1).max(start + 1);
    Some((source.0 + start, source.0 + end))
}

/// Averages the source box; `None` when it is mostly transparent or outside the image.
fn sample(pixels: &Pixels<'_>, x: (u64, u64), y: (u64, u64)) -> Option<[u8; 3]> {
    let (x0, x1) = (
        x.0.min(u64::from(pixels.width)),
        x.1.min(u64::from(pixels.width)),
    );
    let (y0, y1) = (
        y.0.min(u64::from(pixels.height)),
        y.1.min(u64::from(pixels.height)),
    );
    if x0 >= x1 || y0 >= y1 {
        return None;
    }
    let x_step = ((x1 - x0) / MAX_SAMPLES_PER_AXIS).max(1);
    let y_step = ((y1 - y0) / MAX_SAMPLES_PER_AXIS).max(1);
    let (mut r, mut g, mut b, mut a, mut count) = (0u64, 0u64, 0u64, 0u64, 0u64);
    let mut y = y0;
    while y < y1 {
        let mut x = x0;
        while x < x1 {
            let [pr, pg, pb, pa] = pixels.rgba(x as u32, y as u32);
            let alpha = u64::from(pa);
            r += u64::from(pr) * alpha;
            g += u64::from(pg) * alpha;
            b += u64::from(pb) * alpha;
            a += alpha;
            count += 1;
            x += x_step;
        }
        y += y_step;
    }
    if a == 0 || (a / count) < u64::from(OPAQUE_ALPHA) {
        return None;
    }
    Some([(r / a) as u8, (g / a) as u8, (b / a) as u8])
}

pub(crate) fn rasterize(pixels: &Pixels<'_>, geometry: CellGeometry) -> CellImage {
    let cols = geometry.cols;
    let rows = geometry.rows;
    let cell_w = u64::from(geometry.cell_size.width_px.max(1));
    let cell_h = u64::from(geometry.cell_size.height_px.max(1));
    let display_w = u64::from(cols) * cell_w;
    // Rows work in half-pixel units so an odd cell height splits exactly.
    let display_h2 = u64::from(rows) * cell_h * 2;
    let source_w = u64::from(geometry.source_width.max(1));
    let source_h = u64::from(geometry.source_height.max(1));
    let x_span = |col: u64| {
        source_span(
            (col * cell_w, (col + 1) * cell_w),
            display_w,
            u64::from(geometry.x_offset),
            (u64::from(geometry.source_x), source_w),
        )
    };
    let y_span = |half_row: u64| {
        source_span(
            (half_row * cell_h, (half_row + 1) * cell_h),
            display_h2,
            u64::from(geometry.y_offset) * 2,
            (u64::from(geometry.source_y), source_h),
        )
    };
    let mut samples = Vec::with_capacity(cols as usize * rows as usize * 2);
    for half_row in 0..u64::from(rows) * 2 {
        let y = y_span(half_row);
        for col in 0..u64::from(cols) {
            samples.push(x_span(col).zip(y).and_then(|(x, y)| sample(pixels, x, y)));
        }
    }
    CellImage {
        cols,
        rows,
        samples,
    }
}

impl CellImage {
    /// Paints the image over `frame` with its top-left cell at `(x, y)`.
    pub(crate) fn paint(&self, frame: &mut FrameData, x: u16, y: u16) {
        let width = usize::from(frame.width);
        for row in 0..self.rows {
            let Some(frame_y) = u16::try_from(row).ok().and_then(|row| y.checked_add(row)) else {
                break;
            };
            if frame_y >= frame.height {
                break;
            }
            for col in 0..self.cols {
                let Some(frame_x) = u16::try_from(col).ok().and_then(|col| x.checked_add(col))
                else {
                    break;
                };
                if frame_x >= frame.width {
                    break;
                }
                let upper = self.samples[(row as usize * 2) * self.cols as usize + col as usize];
                let lower =
                    self.samples[(row as usize * 2 + 1) * self.cols as usize + col as usize];
                let Some(cell) = frame
                    .cells
                    .get_mut(usize::from(frame_y) * width + usize::from(frame_x))
                else {
                    return;
                };
                let rgb = |[r, g, b]: [u8; 3]| color_to_u32(ratatui::style::Color::Rgb(r, g, b));
                match (upper, lower) {
                    (None, None) => continue,
                    (Some(upper), Some(lower)) => {
                        cell.symbol = "▀".to_owned();
                        cell.fg = rgb(upper);
                        cell.bg = rgb(lower);
                    }
                    // A half block already in the cell holds the other half's
                    // color in `fg`; keep it and fill the remaining half.
                    (Some(upper), None) if cell.symbol == "▄" => cell.bg = rgb(upper),
                    (Some(upper), None) => {
                        cell.symbol = "▀".to_owned();
                        cell.fg = rgb(upper);
                    }
                    (None, Some(lower)) if cell.symbol == "▀" => cell.bg = rgb(lower),
                    (None, Some(lower)) => {
                        cell.symbol = "▄".to_owned();
                        cell.fg = rgb(lower);
                    }
                }
                cell.modifier = 0;
                cell.skip = false;
                cell.hyperlink = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::CellData;

    const RED: [u8; 4] = [255, 0, 0, 255];
    const GREEN: [u8; 4] = [0, 255, 0, 255];
    const BLUE: [u8; 4] = [0, 0, 255, 255];
    const CLEAR: [u8; 4] = [0, 0, 0, 0];

    fn rgba(pixels: &[[u8; 4]]) -> Vec<u8> {
        pixels.iter().flatten().copied().collect()
    }

    fn geometry(cols: u32, rows: u32, width: u32, height: u32) -> CellGeometry {
        CellGeometry {
            cols,
            rows,
            source_x: 0,
            source_y: 0,
            source_width: width,
            source_height: height,
            x_offset: 0,
            y_offset: 0,
            cell_size: HostCellSize {
                width_px: 8,
                height_px: 16,
            },
        }
    }

    fn frame(width: u16, height: u16) -> FrameData {
        FrameData {
            cells: (0..usize::from(width) * usize::from(height))
                .map(|_| CellData {
                    symbol: " ".into(),
                    fg: 7,
                    bg: 0x02_10_20_30,
                    modifier: 1,
                    skip: false,
                    hyperlink: None,
                })
                .collect(),
            width,
            height,
            cursor: None,
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        }
    }

    #[test]
    fn one_cell_shows_the_upper_pixel_as_foreground_and_the_lower_as_background() {
        let data = rgba(&[RED, GREEN, BLUE, BLUE]);
        let pixels = Pixels::new(SurfaceGraphicsFormat::Rgba, 2, 2, &data).unwrap();
        let image = rasterize(&pixels, geometry(1, 1, 2, 2));
        // The top row averages red and green with straight alpha.
        assert_eq!(image.samples, vec![Some([127, 127, 0]), Some([0, 0, 255])]);
        let mut frame = frame(1, 1);
        image.paint(&mut frame, 0, 0);
        let cell = &frame.cells[0];
        assert_eq!(cell.symbol, "▀");
        assert_eq!(cell.fg, 0x02_7F_7F_00);
        assert_eq!(cell.bg, 0x02_00_00_FF);
        assert_eq!(cell.modifier, 0);
    }

    #[test]
    fn transparent_pixels_keep_the_cell_background_and_fully_clear_cells_stay_untouched() {
        let data = rgba(&[RED, CLEAR, CLEAR, CLEAR, BLUE, CLEAR]);
        let pixels = Pixels::new(SurfaceGraphicsFormat::Rgba, 3, 2, &data).unwrap();
        let image = rasterize(&pixels, geometry(3, 1, 3, 2));
        let mut frame = frame(3, 1);
        image.paint(&mut frame, 0, 0);
        assert_eq!(frame.cells[0].symbol, "▀");
        assert_eq!(frame.cells[0].fg, 0x02_FF_00_00);
        assert_eq!(frame.cells[0].bg, 0x02_10_20_30, "lower pixel is clear");
        assert_eq!(frame.cells[1].symbol, "▄");
        assert_eq!(frame.cells[1].fg, 0x02_00_00_FF);
        assert_eq!(frame.cells[1].bg, 0x02_10_20_30, "upper pixel is clear");
        assert_eq!(frame.cells[2].symbol, " ");
        assert_eq!(
            frame.cells[2].modifier, 1,
            "untouched cells keep their style"
        );
    }

    #[test]
    fn odd_image_heights_cover_every_half_row_without_panicking() {
        let data = rgba(&[RED, GREEN, BLUE]);
        let pixels = Pixels::new(SurfaceGraphicsFormat::Rgba, 1, 3, &data).unwrap();
        let image = rasterize(&pixels, geometry(1, 1, 1, 3));
        // Half rows split 3 source rows at 1.5: [0,1) and [1,3) sample red, then green+blue.
        assert_eq!(image.samples, vec![Some([255, 0, 0]), Some([0, 127, 127])]);
        let tall = rasterize(&pixels, geometry(1, 2, 1, 3));
        assert_eq!(tall.samples.len(), 4);
        assert!(tall.samples.iter().all(Option::is_some));
    }

    #[test]
    fn rgb_sources_are_opaque_and_png_sources_decode_to_their_key_dimensions() {
        let data = [255, 0, 0, 0, 0, 255];
        let pixels = Pixels::new(SurfaceGraphicsFormat::Rgb, 1, 2, &data).unwrap();
        let image = rasterize(&pixels, geometry(1, 1, 1, 2));
        assert_eq!(image.samples, vec![Some([255, 0, 0]), Some([0, 0, 255])]);
        assert!(Pixels::new(SurfaceGraphicsFormat::Rgb, 1, 2, &data[..5]).is_none());

        let mut png = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut png, 1, 2);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&data).unwrap();
        }
        let decoded = decode_png(&png, 1, 2).unwrap();
        assert_eq!(&decoded[..], &[255, 0, 0, 255, 0, 0, 255, 255]);
        assert!(decode_png(&png, 2, 1).is_none());
    }

    #[test]
    fn clipped_sources_and_pixel_offsets_follow_the_placement_geometry() {
        let data = rgba(&[RED, GREEN, BLUE, RED, GREEN, BLUE]);
        let pixels = Pixels::new(SurfaceGraphicsFormat::Rgba, 3, 2, &data).unwrap();
        let mut geometry = geometry(1, 1, 1, 2);
        geometry.source_x = 2;
        assert_eq!(
            rasterize(&pixels, geometry).samples,
            vec![Some([0, 0, 255]), Some([0, 0, 255])]
        );
        let mut offset = self::geometry(3, 1, 3, 2);
        offset.x_offset = 8;
        // Shifting the image a full cell right leaves the first cell empty and
        // packs the three columns into the remaining two cells.
        let samples = rasterize(&pixels, offset).samples;
        assert_eq!(samples[0], None);
        assert_eq!(samples[1], Some([255, 0, 0]));
        assert_eq!(samples[2], Some([0, 127, 127]));
    }

    #[test]
    fn painting_half_a_cell_keeps_the_other_half_of_an_earlier_image() {
        let paint = |out: &mut FrameData, pixels: &[[u8; 4]]| {
            let data = rgba(pixels);
            let pixels = Pixels::new(SurfaceGraphicsFormat::Rgba, 1, 2, &data).unwrap();
            rasterize(&pixels, geometry(1, 1, 1, 2)).paint(out, 0, 0);
        };
        let cell = |out: &FrameData| {
            (
                out.cells[0].symbol.clone(),
                out.cells[0].fg,
                out.cells[0].bg,
            )
        };

        let mut out = frame(1, 1);
        paint(&mut out, &[RED, BLUE]);
        paint(&mut out, &[CLEAR, GREEN]);
        assert_eq!(cell(&out), ("▀".to_owned(), 0x02_FF_00_00, 0x02_00_FF_00));

        let mut out = frame(1, 1);
        paint(&mut out, &[CLEAR, BLUE]);
        paint(&mut out, &[RED, CLEAR]);
        assert_eq!(cell(&out), ("▄".to_owned(), 0x02_00_00_FF, 0x02_FF_00_00));
    }

    #[test]
    fn png_dimensions_past_the_decode_budget_are_rejected() {
        // 1-bit grayscale keeps the encoded file small while its RGBA output
        // would pass the budget.
        let (width, height) = (4097, 4096);
        let mut png = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut png, width, height);
            encoder.set_color(png::ColorType::Grayscale);
            encoder.set_depth(png::BitDepth::One);
            let mut writer = encoder.write_header().unwrap();
            writer
                .write_image_data(&vec![0; (width as usize).div_ceil(8) * height as usize])
                .unwrap();
        }
        assert!(decode_png(&png, width, height).is_none());
    }

    #[test]
    fn painting_clips_to_the_frame() {
        let data = rgba(&[RED; 4]);
        let pixels = Pixels::new(SurfaceGraphicsFormat::Rgba, 2, 2, &data).unwrap();
        let image = rasterize(&pixels, geometry(2, 2, 2, 2));
        let mut frame = frame(2, 2);
        image.paint(&mut frame, 1, 1);
        assert_eq!(frame.cells[3].symbol, "▀");
        assert!(frame.cells[..3].iter().all(|cell| cell.symbol == " "));
    }
}
