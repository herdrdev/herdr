use super::*;

fn cell(symbol: &str) -> CellData {
    CellData {
        symbol: symbol.to_owned(),
        fg: 0,
        bg: 0,
        modifier: 0,
        skip: false,
        hyperlink: None,
    }
}

fn row(text: &str) -> Vec<CellData> {
    text.chars().map(|c| cell(&c.to_string())).collect()
}

fn text(cells: &[CellData]) -> String {
    cells.iter().map(|cell| cell.symbol.as_str()).collect()
}

fn symbols(cells: &[CellData]) -> Vec<&str> {
    cells.iter().map(|cell| cell.symbol.as_str()).collect()
}

/// Lays out `cells` and returns the column map when cells moved.
fn layout(cells: &mut [CellData]) -> Option<Box<[u16]>> {
    layout_segment(cells, &mut Scratch::default())
}

/// Lays out `input` and checks the returned map against the cells.
fn visual(input: &str) -> String {
    let logical = row(input);
    let mut cells = logical.clone();
    if let Some(map) = layout(&mut cells) {
        assert_is_permutation(&map);
        let row_map = RowMap::new(0, 0, map.clone());
        for (visual, &source) in map.iter().enumerate() {
            assert_eq!(row_map.to_logical(visual as u16), source);
            assert_eq!(row_map.to_visual(source), visual as u16);
        }
    }
    text(&cells)
}

fn assert_is_permutation(map: &[u16]) {
    let mut sorted = map.to_vec();
    sorted.sort_unstable();
    assert!(sorted.iter().enumerate().all(|(i, &v)| usize::from(v) == i));
}

#[test]
fn pure_hebrew_is_drawn_right_to_left() {
    assert_eq!(visual("שלום"), "םולש");
}

#[test]
fn latin_and_hebrew_keep_a_left_to_right_base() {
    // W7, N1 and I1: a number after a Hebrew word joins its right-to-left
    // run, so it is drawn to the word's left, as browsers do.
    assert_eq!(visual("abc אבג 123"), "abc 123 גבא");
    assert_eq!(visual("abc שלום 123 עולם xyz"), "abc םלוע 123 םולש xyz");
}

#[test]
fn issue_example_reads_in_order() {
    assert_eq!(
        visual("קובץ config.toml לא נמצא"),
        "ץבוק config.toml אצמנ אל"
    );
}

#[test]
fn numbers_after_hebrew_and_trailing_padding() {
    assert_eq!(visual("אב 12"), "12 בא");
    assert_eq!(visual("שלום     "), "םולש     ");
}

#[test]
fn combining_marks_stay_on_their_base_cell() {
    let mut cells = vec![cell("a"), cell(" "), cell("שָׁ"), cell("לוֹ"), cell("ם")];
    layout(&mut cells);
    assert_eq!(symbols(&cells), vec!["a", " ", "ם", "לוֹ", "שָׁ"]);

    let mut arabic = vec![cell("x"), cell(" "), cell("بِ"), cell("سْ"), cell("م")];
    layout(&mut arabic);
    assert_eq!(symbols(&arabic), vec!["x", " ", "م", "سْ", "بِ"]);
}

#[test]
fn wide_graphemes_keep_head_then_tail() {
    let mut cells = vec![cell("א"), cell("😀"), cell(""), cell("ב")];
    let map = layout(&mut cells).expect("reordered");
    assert_eq!(symbols(&cells), vec!["ב", "😀", "", "א"]);
    assert_eq!(&*map, &[3, 1, 2, 0]);

    let mut cjk = vec![cell("中"), cell(""), cell(" "), cell("א"), cell("ב")];
    layout(&mut cjk);
    assert_eq!(symbols(&cjk), vec!["中", "", " ", "ב", "א"]);
}

#[test]
fn brackets_are_mirrored_inside_right_to_left_runs() {
    assert_eq!(visual("(שלום)"), "(םולש)");
    assert_eq!(visual("א (ב) ג"), "ג (ב) א");
    assert_eq!(visual("א «ב» ג"), "ג «ב» א");
}

#[test]
fn box_drawing_frames_split_paragraphs_and_never_move() {
    assert_eq!(visual("│ שלום │ עולם │"), "│ םולש │ םלוע │");
    assert_eq!(visual("─אב─גד─"), "─בא─דג─");
}

#[test]
fn rows_without_right_to_left_text_are_untouched() {
    let mut scratch = Scratch::default();
    for input in ["abc 123 xyz", "中文 テスト", "┌──┐ │ab│ └──┘", "café ñ"]
    {
        let mut cells = row(input);
        assert!(!affects(&cells), "{input}");
        assert!(layout_segment(&mut cells, &mut scratch).is_none());
        assert_eq!(text(&cells), input);
    }
    assert_eq!(scratch.text.capacity(), 0);
    assert_eq!(scratch.cells.capacity(), 0);
}

#[test]
fn skip_cells_and_kitty_placeholders_block_the_transform() {
    let mut skipped = row("אב");
    skipped[1].skip = true;
    assert!(!affects(&skipped));

    let mut image = row("אב");
    image.push(cell("\u{10EEEE}"));
    assert!(!affects(&image));
    assert!(layout(&mut image).is_none());
}

#[test]
fn explicit_formatting_characters_follow_uax9() {
    // Lines adapted from BidiCharacterTest.txt with paragraph direction 0.
    // RLO forces the Latin letters right to left. Rule X9 removes the
    // embedding controls, so their zero-width cells keep their position.
    assert_eq!(visual("x\u{202E}ab\u{202C}y"), "x\u{202E}\u{202C}bay");
    // RLI isolates its content. The isolate controls take the outer level
    // (X5a, X6a), so only the isolated content is reversed.
    assert_eq!(visual("a \u{2067}אב\u{2069} c"), "a \u{2067}בא\u{2069} c");
    // RLM alone between Latin words resolves to an isolated R run.
    assert_eq!(visual("ab\u{200F}cd"), "ab\u{200F}cd");
    // Arabic-Indic digits keep their left-to-right order next to Arabic.
    assert_eq!(visual("س ١٢"), "١٢ س");
    // European digits inside a Hebrew run keep their order.
    assert_eq!(visual("א 12 ב"), "ב 12 א");
}

#[test]
fn styles_move_with_their_glyphs() {
    let mut cells = row("אב");
    cells[0].fg = 1;
    cells[1].fg = 2;
    layout(&mut cells);
    assert_eq!((cells[0].symbol.as_str(), cells[0].fg), ("ב", 2));
    assert_eq!((cells[1].symbol.as_str(), cells[1].fg), ("א", 1));
}

#[test]
fn leading_orphan_spacer_is_kept() {
    let mut cells = vec![cell(""), cell("א"), cell("ב")];
    layout(&mut cells);
    assert_eq!(symbols(&cells), vec!["", "ב", "א"]);
}

#[test]
fn invalid_layout_output_restores_the_logical_row() {
    let logical = vec![cell("a"), cell("中"), cell(""), cell("b")];
    let mut cells = logical.clone();
    // Splits the wide head from its tail and duplicates column 0.
    let broken = |segment: &mut [CellData], map: &mut Vec<u16>, _: &mut Scratch| {
        segment.reverse();
        map.extend(std::iter::repeat_n(0, segment.len()));
        true
    };
    assert!(layout_with(&mut cells, &mut Scratch::default(), broken).is_none());
    assert_eq!(cells, logical);

    // A bijection that still splits a wide grapheme is rejected too.
    let mut cells = logical.clone();
    let split = |segment: &mut [CellData], map: &mut Vec<u16>, _: &mut Scratch| {
        segment.reverse();
        map.extend([3, 2, 1, 0]);
        true
    };
    assert!(layout_with(&mut cells, &mut Scratch::default(), split).is_none());
    assert_eq!(cells, logical);
}

#[test]
fn row_map_maps_only_inside_its_span() {
    let map = RowMap::new(10, 3, vec![2, 1, 0].into_boxed_slice());
    assert!(map.contains(10, 3) && map.contains(12, 3));
    assert!(!map.contains(13, 3) && !map.contains(10, 4) && !map.contains(9, 3));
    assert_eq!(map.to_logical(10), 12);
    assert_eq!(map.to_visual(12), 10);
    assert_eq!(map.to_logical(20), 20);
}
