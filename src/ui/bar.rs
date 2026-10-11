//! Shared terminal bar glyph selection for usage charts and quota meters.

const BAR_GLYPHS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

pub(super) fn bar_glyph(value: f64, maximum: f64) -> char {
    if maximum <= 0.0 {
        return BAR_GLYPHS[0];
    }
    let index = ((value / maximum) * 7.0).round().clamp(0.0, 7.0) as usize;
    BAR_GLYPHS[index]
}

#[cfg(test)]
pub(super) fn is_bar_glyph(glyph: char) -> bool {
    BAR_GLYPHS.contains(&glyph)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chart_glyph_scale_matches_the_existing_eight_steps() {
        assert_eq!(bar_glyph(0.0, 0.0), '▁');
        assert_eq!(bar_glyph(0.0, 8.0), '▁');
        assert_eq!(bar_glyph(4.0, 8.0), '▅');
        assert_eq!(bar_glyph(8.0, 8.0), '█');
    }
}
