#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Position {
    Cell { column: u16, row: u16 },
    Pixels { x: u32, y: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HostGeometry {
    pub(crate) cols: u16,
    pub(crate) rows: u16,
    pub(crate) width_px: u32,
    pub(crate) height_px: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HostPixels {
    pub(crate) x: u32,
    pub(crate) y: u32,
    pub(crate) geometry: HostGeometry,
}

impl HostGeometry {
    pub(crate) fn new(cols: u16, rows: u16, width_px: u32, height_px: u32) -> Option<Self> {
        (cols > 0 && rows > 0 && width_px > 0 && height_px > 0).then_some(Self {
            cols,
            rows,
            width_px,
            height_px,
        })
    }

    #[cfg(unix)]
    pub(crate) fn current() -> Option<Self> {
        let size = crossterm::terminal::window_size().ok()?;
        Self::new(
            size.columns,
            size.rows,
            u32::from(size.width),
            u32::from(size.height),
        )
    }

    pub(crate) fn cell(self, x: u32, y: u32) -> Option<(u16, u16)> {
        Some((
            grid_cell(x.checked_sub(1)?, self.cols, self.width_px)?,
            grid_cell(y.checked_sub(1)?, self.rows, self.height_px)?,
        ))
    }

    fn column_boundary(self, column: u16) -> Option<u32> {
        boundary(column, self.cols, self.width_px)
    }

    fn row_boundary(self, row: u16) -> Option<u32> {
        boundary(row, self.rows, self.height_px)
    }
}

impl HostPixels {
    pub(crate) fn pane_position(
        self,
        inner: ratatui::layout::Rect,
        child_width_px: u32,
        child_height_px: u32,
    ) -> Option<Position> {
        let start_x = self.geometry.column_boundary(inner.x)?;
        let start_y = self.geometry.row_boundary(inner.y)?;
        let end_x = self
            .geometry
            .column_boundary(inner.x.checked_add(inner.width)?)?;
        let end_y = self
            .geometry
            .row_boundary(inner.y.checked_add(inner.height)?)?;
        let x = self.x.checked_sub(1)?.checked_sub(start_x)?;
        let y = self.y.checked_sub(1)?.checked_sub(start_y)?;
        let source_width = end_x.checked_sub(start_x)?;
        let source_height = end_y.checked_sub(start_y)?;
        if x >= source_width || y >= source_height || child_width_px == 0 || child_height_px == 0 {
            return None;
        }
        Some(Position::Pixels {
            x: scale(x, source_width, child_width_px).checked_add(1)?,
            y: scale(y, source_height, child_height_px).checked_add(1)?,
        })
    }
}

pub(crate) fn parse_report(data: &[u8]) -> Option<(u32, u32)> {
    let body = data.strip_prefix(b"\x1b[<")?;
    let body = body
        .strip_suffix(b"M")
        .or_else(|| body.strip_suffix(b"m"))?;
    let mut fields = body.split(|byte| *byte == b';');
    parse_number(fields.next()?)?;
    let x = parse_number(fields.next()?)?;
    let y = parse_number(fields.next()?)?;
    fields.next().is_none().then_some((x, y))
}

pub(crate) fn report_at_cell(data: &[u8], column: u16, row: u16) -> Option<Vec<u8>> {
    let body = data.strip_prefix(b"\x1b[<")?;
    let suffix = if body.ends_with(b"M") { 'M' } else { 'm' };
    let body = body.strip_suffix(&[suffix as u8])?;
    let buttons = body.split(|byte| *byte == b';').next()?;
    Some(
        format!(
            "\x1b[<{};{};{}{}",
            std::str::from_utf8(buttons).ok()?,
            u32::from(column) + 1,
            u32::from(row) + 1,
            suffix
        )
        .into_bytes(),
    )
}

fn parse_number(value: &[u8]) -> Option<u32> {
    (!value.is_empty() && value.iter().all(u8::is_ascii_digit))
        .then(|| std::str::from_utf8(value).ok()?.parse().ok())
        .flatten()
}

fn boundary(index: u16, count: u16, extent: u32) -> Option<u32> {
    (count > 0 && index <= count && extent > 0)
        .then(|| (u64::from(index) * u64::from(extent) / u64::from(count)) as u32)
}

fn grid_cell(pixel: u32, count: u16, extent: u32) -> Option<u16> {
    if count == 0 || extent == 0 || pixel >= extent {
        return None;
    }
    let cell = ((u64::from(pixel) + 1) * u64::from(count) - 1) / u64::from(extent);
    u16::try_from(cell).ok().filter(|cell| *cell < count)
}

fn scale(pixel: u32, source: u32, target: u32) -> u32 {
    ((u64::from(pixel) * u64::from(target)) / u64::from(source))
        .min(u64::from(target.saturating_sub(1))) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_accepts_only_complete_sgr_mouse_reports() {
        for (input, expected) in [
            (b"\x1b[<35;321;241M".as_slice(), Some((321, 241))),
            (b"\x1b[<0;1;2m".as_slice(), Some((1, 2))),
            (b"key".as_slice(), None),
            (b"\x1b[<0;1;2Mkey".as_slice(), None),
            (b"\x1b[<0;1M".as_slice(), None),
        ] {
            assert_eq!(parse_report(input), expected);
        }
    }

    #[test]
    fn fractional_geometry_maps_exactly_to_pane_pixels() {
        let geometry = HostGeometry::new(211, 57, 2_537, 1_429).unwrap();
        let inner = ratatui::layout::Rect::new(157, 7, 53, 49);
        let start_x = geometry.column_boundary(inner.x).unwrap();
        let end_x = geometry.column_boundary(inner.x + inner.width).unwrap();
        let start_y = geometry.row_boundary(inner.y).unwrap();
        let end_y = geometry.row_boundary(inner.y + inner.height).unwrap();
        assert_eq!(
            HostPixels {
                x: start_x + 1,
                y: start_y + 1,
                geometry,
            }
            .pane_position(inner, 636, 1_225),
            Some(Position::Pixels { x: 1, y: 1 })
        );
        assert_eq!(
            HostPixels {
                x: end_x,
                y: end_y,
                geometry,
            }
            .pane_position(inner, 636, 1_225),
            Some(Position::Pixels { x: 636, y: 1_225 })
        );
    }

    #[test]
    fn geometry_rejects_outside_pixels_and_maps_cells() {
        let geometry = HostGeometry::new(80, 24, 800, 480).unwrap();
        assert_eq!(geometry.cell(1, 1), Some((0, 0)));
        assert_eq!(geometry.cell(800, 480), Some((79, 23)));
        assert_eq!(geometry.cell(801, 1), None);
        assert_eq!(geometry.cell(0, 1), None);
    }
}
