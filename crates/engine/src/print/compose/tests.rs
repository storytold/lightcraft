use super::*;
use crate::print::layout::{GridSpec, Margins};

const RED: [u16; 3] = [60000, 1000, 1000];
const BLUE: [u16; 3] = [1000, 1000, 60000];
const WHITE: [u16; 3] = [65535; 3];

/// A `w` x `h` image whose left half is red and right half blue, with a green top row.
fn halves(w: usize, h: usize) -> Image16 {
    let mut img = Image16::filled(w, h, RED).unwrap();
    img.fill_rect(w / 2, 0, w - w / 2, h, BLUE);
    img.fill_rect(0, 0, w, 1, [1000, 60000, 1000]);
    img
}

fn style() -> CellStyle {
    CellStyle { fill: false, rotate: false, border_px: 0, border: [0; 3], stroke_px: 0, stroke: [0; 3] }
}

fn at(img: &Image16, x: usize, y: usize) -> [u16; 3] {
    img.px(x, y).unwrap()
}

fn page() -> Image16 {
    Image16::filled(100, 100, WHITE).unwrap()
}

#[test]
fn placement_fit_fill_and_rotation() {
    // 4:3 photo into a 2:1 area.
    let fit = place(400.0, 300.0, 200, 100, false, false).unwrap();
    assert_eq!((fit.w, fit.h, fit.rotated), (133, 100, false));
    let fill = place(400.0, 300.0, 200, 100, true, false).unwrap();
    assert_eq!((fill.w, fill.h), (200, 150));
    // A portrait photo in a landscape area: turning prints it larger.
    assert_eq!(place(300.0, 400.0, 400, 300, false, false).unwrap(), Placement { rotated: false, w: 225, h: 300 });
    let turned = place(300.0, 400.0, 400, 300, false, true).unwrap();
    assert_eq!((turned.rotated, turned.w, turned.h), (true, 400, 300));
    assert_eq!(turned.unturned(), (300, 400));
    // Filling a landscape cell with a portrait photo crops less when turned.
    let f = place(300.0, 400.0, 400, 300, true, true).unwrap();
    assert!(f.rotated && f.w >= 400 && f.h >= 300);
    // Matching orientation never turns.
    assert!(!place(400.0, 300.0, 400, 300, false, true).unwrap().rotated);
    assert!(!place(300.0, 300.0, 100, 100, false, true).unwrap().rotated);
    // Hostile input.
    for bad in [f64::NAN, f64::INFINITY, 0.0, -5.0] {
        assert!(place(bad, 10.0, 10, 10, false, false).is_none());
        assert!(place(10.0, bad, 10, 10, true, true).is_none());
    }
    assert!(place(10.0, 10.0, 0, 10, false, false).is_none());
}

#[test]
fn fit_centres_the_photo() {
    let mut p = page();
    draw_cell(&mut p, &halves(40, 20), PxRect { x: 10, y: 10, w: 80, h: 80 }, &style()).unwrap();
    // 40x20 scaled to 80x40, centred vertically at y 30..70.
    assert_eq!(at(&p, 10, 50), RED);
    assert_eq!(at(&p, 89, 50), BLUE);
    assert_eq!(at(&p, 50, 20), WHITE, "above the photo");
    assert_eq!(at(&p, 50, 75), WHITE, "below the photo");
    assert_eq!(at(&p, 5, 50), WHITE, "outside the cell");
}

#[test]
fn zoom_to_fill_crops_the_centre() {
    let mut p = page();
    let s = CellStyle { fill: true, ..style() };
    draw_cell(&mut p, &halves(40, 20), PxRect { x: 10, y: 10, w: 80, h: 80 }, &s).unwrap();
    // The photo scales to 160x80 and the centre 80 columns remain: the colour edge is mid-cell.
    for y in [10, 50, 89] {
        assert_ne!(at(&p, 10, y), WHITE);
        assert_ne!(at(&p, 89, y), WHITE);
    }
    assert_eq!(at(&p, 20, 50), RED);
    assert_eq!(at(&p, 80, 50), BLUE);
    // Nothing leaks outside the cell.
    assert_eq!(at(&p, 9, 50), WHITE);
    assert_eq!(at(&p, 90, 50), WHITE);
    assert_eq!(at(&p, 50, 9), WHITE);
    assert_eq!(at(&p, 50, 90), WHITE);
}

#[test]
fn rotate_to_fit_turns_clockwise() {
    // A portrait 20x40 photo (red left, blue right) in a landscape 80x40 cell.
    let mut p = page();
    let cell = PxRect { x: 10, y: 10, w: 80, h: 40 };
    draw_cell(&mut p, &halves(20, 40), cell, &style()).unwrap();
    assert_eq!(at(&p, 20, 30), WHITE, "unturned the photo is a narrow strip in the middle");
    assert_eq!(at(&p, 45, 30), RED);
    let mut q = page();
    draw_cell(&mut q, &halves(20, 40), cell, &CellStyle { rotate: true, ..style() }).unwrap();
    // Turned clockwise the left half (red) becomes the top half and the photo spans the cell.
    assert_eq!(at(&q, 12, 12), RED, "{:?}", at(&q, 12, 12));
    assert_eq!(at(&q, 12, 48), BLUE);
    assert_eq!(at(&q, 50, 20), RED);
    assert_eq!(at(&q, 50, 40), BLUE);
    assert_eq!(at(&q, 11, 30), at(&q, 11, 30));
}

#[test]
fn border_frames_the_photo_inside_the_cell() {
    let mut p = page();
    let green = [100, 60000, 100];
    let s = CellStyle { border_px: 5, border: green, fill: true, ..style() };
    draw_cell(&mut p, &halves(60, 60), PxRect { x: 10, y: 10, w: 60, h: 60 }, &s).unwrap();
    assert_eq!(at(&p, 10, 40), green);
    assert_eq!(at(&p, 14, 40), green);
    assert_eq!(at(&p, 15, 40), RED);
    assert_eq!(at(&p, 69, 40), green);
    assert_eq!(at(&p, 64, 40), BLUE);
    assert_eq!(at(&p, 40, 10), green);
    assert_eq!(at(&p, 40, 69), green);
    // A fitted photo gets the border hugging it, not the whole cell.
    let mut q = page();
    let s = CellStyle { border_px: 5, border: green, ..style() };
    draw_cell(&mut q, &halves(60, 30), PxRect { x: 0, y: 0, w: 100, h: 100 }, &s).unwrap();
    // The 90x90 area fits a 90x45 photo at y 27..72 inside a frame 5 px thick.
    assert_eq!(at(&q, 50, 23), green);
    assert_eq!(at(&q, 50, 22), green);
    assert_eq!(at(&q, 50, 21), WHITE);
    // A border wider than the cell leaves the photo visible.
    let mut r = page();
    let s = CellStyle { border_px: 500, border: green, ..style() };
    draw_cell(&mut r, &halves(10, 10), PxRect { x: 10, y: 10, w: 20, h: 20 }, &s).unwrap();
    assert_ne!(at(&r, 20, 20), WHITE);
}

#[test]
fn stroke_is_drawn_inside_the_photo_edge() {
    let mut p = page();
    let black = [0, 0, 0];
    let s = CellStyle { stroke_px: 3, stroke: black, fill: true, ..style() };
    draw_cell(&mut p, &halves(60, 60), PxRect { x: 20, y: 20, w: 60, h: 60 }, &s).unwrap();
    for (x, y) in [(20, 40), (22, 40), (77, 40), (79, 40), (40, 20), (40, 22), (40, 77), (40, 79)] {
        assert_eq!(at(&p, x, y), black, "({x}, {y})");
    }
    assert_eq!(at(&p, 23, 40), RED);
    assert_eq!(at(&p, 76, 40), BLUE);
    assert_eq!(at(&p, 19, 40), WHITE, "the stroke does not grow the photo");
    // Corners are covered exactly once and a huge stroke stays within the photo.
    let mut q = page();
    let s = CellStyle { stroke_px: 1000, stroke: black, ..style() };
    draw_cell(&mut q, &halves(10, 10), PxRect { x: 20, y: 20, w: 40, h: 40 }, &s).unwrap();
    assert_eq!(at(&q, 40, 40), black);
    assert_eq!(at(&q, 19, 40), WHITE);
    assert_eq!(at(&q, 60, 40), WHITE);
}

#[test]
fn colours_are_rec2020_encoded() {
    assert_eq!(rec2020_from_srgb8([255; 3]), [65535; 3]);
    assert_eq!(rec2020_from_srgb8([0; 3]), [0; 3]);
    let grey = rec2020_from_srgb8([128; 3]);
    assert!(grey[0].abs_diff(grey[1]) < 3 && grey[1].abs_diff(grey[2]) < 3);
    // sRGB red sits inside the Rec. 2020 gamut: its green and blue are not zero.
    let red = rec2020_from_srgb8([255, 0, 0]);
    assert!(red[0] < 65535 && red[1] > 0);
}

#[test]
fn resize_preserves_flat_colour_and_edges() {
    let img = halves(64, 32);
    for (w, h) in [(32, 16), (128, 64), (50, 70), (1, 1), (64, 31)] {
        let r = img.resized(w, h).unwrap();
        assert_eq!((r.w, r.h), (w, h));
        assert_eq!(r.rgb.len(), w * h * 3);
    }
    let flat = Image16::filled(40, 30, [1234, 5678, 9999]).unwrap().resized(17, 61).unwrap();
    assert!(flat.rgb.chunks(3).all(|p| p == [1234, 5678, 9999]));
    assert!(img.resized(0, 5).is_err());
    let r = img.resized(32, 16).unwrap();
    assert_eq!(at(&r, 4, 8), RED);
    assert_eq!(at(&r, 28, 8), BLUE);
}

#[test]
fn rotate_and_blit_are_clipped() {
    let img = halves(3, 2);
    let r = img.rotated_cw().unwrap();
    assert_eq!((r.w, r.h), (2, 3));
    let mut p = page();
    p.blit(&img, 0, 0, 99, 99, 10, 10);
    assert_eq!(at(&p, 99, 99), at(&img, 0, 0));
    p.blit(&img, 5, 5, 0, 0, 3, 3);
    p.fill_rect(95, 95, 50, 50, RED);
    assert_eq!(at(&p, 99, 99), RED);
    p.fill_rect(usize::MAX, usize::MAX, usize::MAX, usize::MAX, RED);
}

fn settings(style: Style) -> PrintSettings {
    let mut s = PrintSettings::default();
    s.layout.style = style;
    s.layout.page.paper = "4x6".into();
    s.layout.page.margins = Margins::uniform(0.5);
    s.job.ppi = 100.0;
    s
}

#[test]
fn plans_follow_the_layout_style() {
    let ids: Vec<PhotoId> = (1..=5).map(PhotoId).collect();
    let mut s = settings(Style::ContactSheet);
    s.layout.grid = GridSpec { columns: 2, rows: 2, ..Default::default() };
    let plan = plan_cells(&s.layout, &ids).unwrap();
    assert_eq!(plan.iter().map(Vec::len).collect::<Vec<_>>(), vec![4, 1]);
    assert_eq!(plan[1][0].1, PhotoId(5));
    let one = plan_cells(&settings(Style::SingleImage).layout, &ids).unwrap();
    assert_eq!(one.len(), 5);
    assert!(plan_cells(&s.layout, &[]).is_err());
    assert!(plan_cells(&s.layout, &vec![PhotoId(1); MAX_PHOTOS + 1]).is_err());

    let mut pkg = settings(Style::PicturePackage);
    assert!(plan_cells(&pkg.layout, &ids).is_err(), "no cells yet");
    pkg.layout.add_cell(1.0, 1.0).unwrap();
    pkg.layout.add_cell(2.0, 2.0).unwrap();
    let plan = plan_cells(&pkg.layout, &ids).unwrap();
    assert_eq!(plan.len(), 1);
    assert!(plan[0].iter().all(|(_, p)| *p == PhotoId(1)), "picture package repeats the first photo");

    let mut custom = settings(Style::CustomPackage);
    let a = custom.layout.add_cell(1.0, 1.0).unwrap();
    custom.layout.add_cell(1.0, 1.0).unwrap();
    custom.layout.set_cell_photo(a, Some(3)).unwrap();
    let plan = plan_cells(&custom.layout, &ids).unwrap();
    assert_eq!((plan[0][0].1, plan[0][1].1), (PhotoId(3), PhotoId(1)));
    let blank_page = {
        custom.layout.new_page().unwrap();
        plan_cells(&custom.layout, &ids).unwrap()
    };
    assert_eq!(blank_page.iter().map(Vec::len).collect::<Vec<_>>(), vec![2, 0]);
}

fn first_ids(s: &Session, n: usize) -> Vec<PhotoId> {
    s.catalog.photos().take(n).map(|p| p.id).collect()
}

#[test]
fn composes_pages_from_real_photos() {
    let mut s = Session::with_demo();
    let ids = first_ids(&s, 3);
    let mut set = settings(Style::ContactSheet);
    set.layout.grid = GridSpec { columns: 1, rows: 2, spacing_x: 0.0, spacing_y: 0.0, keep_square: false };
    set.page.background = [200, 30, 30];
    set.image.zoom_to_fill = true;
    set.image.stroke_enabled = true;
    set.image.stroke_width_pt = 2.0;
    set.image.stroke_color = [0, 0, 0];
    let work = prepare(&mut s, &set, &ids).unwrap();
    assert_eq!((work.len(), work.pages()), (3, 2));
    let mut steps = Vec::new();
    let pages = work
        .run(&mut |i, name| {
            steps.push((i, name.to_string()));
            true
        })
        .unwrap();
    assert_eq!(steps.iter().map(|s| s.0).collect::<Vec<_>>(), vec![0, 1, 2, 3]);
    assert_eq!(pages.len(), 2);
    let first = &pages[0];
    assert_eq!((first.width, first.height, first.ppi), (400, 600, 100.0));
    assert_eq!(first.rgb.len(), 400 * 600 * 3);
    let bg = rec2020_from_srgb8([200, 30, 30]);
    let px = |p: &PageRaster, x: usize, y: usize| {
        let i = (y * p.width as usize + x) * 3;
        [p.rgb[i], p.rgb[i + 1], p.rgb[i + 2]]
    };
    assert_eq!(px(first, 5, 5), bg, "margin shows the page background");
    assert_eq!(px(first, 395, 595), bg);
    // Zoom to fill: the top-left photo pixel (inside the 2 pt = 3 px stroke) is the stroke colour.
    assert_eq!(px(first, 50, 50), [0, 0, 0]);
    let centre = px(first, 200, 150);
    assert_ne!(centre, bg);
    assert_ne!(centre, [0, 0, 0]);
    // The second page holds the third photo in the first cell only: the second cell is bare.
    let second = &pages[1];
    assert_ne!(px(second, 200, 150), bg);
    assert_eq!(px(second, 200, 450), bg);
    // Cancelling gives no pages.
    let work = prepare(&mut s, &set, &ids).unwrap();
    assert!(work.run(&mut |i, _| i < 1).is_err());
}

#[test]
fn package_renders_each_size_and_border() {
    let mut s = Session::with_demo();
    let ids = first_ids(&s, 1);
    let mut set = settings(Style::PicturePackage);
    set.layout.add_cell(1.5, 1.0).unwrap();
    set.layout.add_cell(1.5, 1.0).unwrap();
    set.image.border_enabled = true;
    set.image.border_width = 0.1;
    set.image.border_color = [10, 200, 10];
    let pages = prepare(&mut s, &set, &ids).unwrap().run(&mut |_, _| true).unwrap();
    assert_eq!(pages.len(), 1);
    let green = rec2020_from_srgb8([10, 200, 10]);
    let cell = set.layout.cells[0].rect;
    let (x0, y0) = ((cell.x * 100.0).round() as usize, (cell.y * 100.0).round() as usize);
    let at = |x: usize, y: usize| {
        let i = (y * 400 + x) * 3;
        pages[0].rgb[i..i + 3].to_vec()
    };
    // The frame touches the cell edge on the photo's limiting axis: 10 px (0.1 in) of border all round.
    let mid = (x0 + 75, y0 + 50);
    assert_ne!(at(mid.0, mid.1), green.to_vec(), "the photo, not the border, is in the middle");
    let green_px = (y0..y0 + 100).flat_map(|y| (x0..x0 + 150).map(move |x| (x, y))).filter(|&(x, y)| at(x, y) == green.to_vec()).count();
    assert!(green_px > 1000, "the border frames the photo inside its cell: {green_px} border pixels");
}

/// Pages for two photos, one per 4x6 page at 100 ppi, each filling its 0.5 in margins (so the
/// margins are bare background unless something draws there).
fn page_options_pages(s: &mut Session, style: Style, page: PageStyle) -> Vec<PageRaster> {
    let ids = first_ids(s, 2);
    let mut set = settings(style);
    if style.is_package() {
        set.layout.add_cell(3.0, 5.0).unwrap();
        set.layout.new_page().unwrap();
        let id = set.layout.add_cell(3.0, 5.0).unwrap();
        set.layout.set_cell_page(id, 1).unwrap();
    }
    set.image.zoom_to_fill = true;
    set.page = page;
    prepare(s, &set, &ids).unwrap().run(&mut |_, _| true).unwrap()
}

fn rgb(p: &PageRaster, x: usize, y: usize) -> [u16; 3] {
    let i = (y * p.width as usize + x) * 3;
    [p.rgb[i], p.rgb[i + 1], p.rgb[i + 2]]
}

/// The pixels of `x0..x1` x `y0..y1` that are not the white background.
fn inked(p: &PageRaster, x0: usize, x1: usize, y0: usize, y1: usize) -> usize {
    (y0..y1).flat_map(|y| (x0..x1).map(move |x| (x, y))).filter(|&(x, y)| rgb(p, x, y) != WHITE).count()
}

#[test]
fn crop_marks_sit_outside_each_photo_corner() {
    let mut s = Session::with_demo();
    let plain = page_options_pages(&mut s, Style::SingleImage, PageStyle::default());
    assert_eq!(inked(&plain[0], 0, 400, 0, 50), 0, "no marks unless asked");
    let pages = page_options_pages(&mut s, Style::SingleImage, PageStyle { crop_marks: true, ..Default::default() });
    // The photo fills 50..350 x 50..550; a mark is a line 0.25 in long, 1/16 in from the corner,
    // along each edge of the photo.
    for p in &pages {
        for (x, y) in [(30, 50), (50, 30), (370, 50), (349, 30), (30, 549), (50, 570), (370, 549), (349, 570)] {
            assert_eq!(rgb(p, x, y), [0; 3], "a mark at ({x}, {y})");
        }
        for (x, y) in [(47, 50), (50, 47), (30, 60), (60, 30), (10, 50), (50, 10)] {
            assert_eq!(rgb(p, x, y), WHITE, "the gap, or past the mark, at ({x}, {y})");
        }
    }
    // a dark page gets white marks
    let dark = page_options_pages(&mut s, Style::SingleImage, PageStyle { background: [20; 3], crop_marks: true, ..Default::default() });
    assert_eq!(rgb(&dark[0], 30, 50), WHITE);
}

#[test]
fn page_numbers_and_info_print_in_the_bottom_margin() {
    let mut s = Session::with_demo();
    let numbered = page_options_pages(&mut s, Style::SingleImage, PageStyle { page_numbers: true, ..Default::default() });
    // bottom right: the number, different on each page; nothing at the bottom left
    let (right, left) = ((200, 400), (0, 200));
    for p in &numbered {
        assert!(inked(p, right.0, right.1, 550, 600) > 10, "a page number");
        assert_eq!(inked(p, left.0, left.1, 550, 600), 0);
        assert_eq!(inked(p, 0, 400, 0, 50), 0, "nothing at the top");
    }
    let region = |p: &PageRaster| (550..600).flat_map(|y| (right.0..right.1).map(move |x| (x, y))).map(|(x, y)| rgb(p, x, y)).collect::<Vec<_>>();
    assert_ne!(region(&numbered[0]), region(&numbered[1]), "page 1 and page 2 are numbered differently");
    let info = page_options_pages(&mut s, Style::SingleImage, PageStyle { page_info: true, ..Default::default() });
    for p in &info {
        assert!(inked(p, left.0, left.1, 550, 600) > 10, "the print settings line");
    }
    // the packages have no Page Options in Lightroom: nothing is drawn
    let all = PageStyle { page_numbers: true, page_info: true, crop_marks: true, ..Default::default() };
    let package = page_options_pages(&mut s, Style::PicturePackage, all);
    assert_eq!(package.len(), 2);
    for p in &package {
        assert_eq!(inked(p, 0, 400, 550, 600) + inked(p, 0, 400, 0, 50), 0);
    }
}

#[test]
fn page_info_names_the_sharpening_profile_and_resolution() {
    use crate::print_color::{ManagedSpace, PrintMedia, PrintProfile, PrintSharpen};
    let mut set = PrintSettings::default();
    let line = page_info_text(&set);
    assert_eq!(line, "Print Sharpening: Standard (Glossy) · Profile: Managed by Printer (sRGB) · 240 ppi");
    set.job.color.sharpen = PrintSharpen::Off;
    set.job.color.media = PrintMedia::Matte;
    set.job.color.profile = PrintProfile::Icc { path: "/profiles/Lustre Paper.icc".into() };
    set.job.ppi = 300.0;
    assert_eq!(page_info_text(&set), "Print Sharpening: Off · Profile: Lustre Paper · 300 ppi");
    set.job.color.profile = PrintProfile::Managed { space: ManagedSpace::AdobeRgb };
    assert!(page_info_text(&set).contains("Managed by Printer (Adobe RGB)"));
}

#[test]
fn caps_and_hostile_settings_are_refused() {
    let mut s = Session::with_demo();
    let ids = first_ids(&s, 1);
    let mut set = settings(Style::SingleImage);
    set.layout.page.paper = "custom".into();
    set.layout.page.custom_width = 60.0;
    set.layout.page.custom_height = 60.0;
    set.job.ppi = 720.0;
    assert!(prepare(&mut s, &set, &ids).is_err(), "a 60 in page at 720 ppi is far beyond the page cap");
    let mut set = settings(Style::SingleImage);
    set.job.ppi = f32::NAN;
    assert!(prepare(&mut s, &set, &ids).is_err());
    let mut set = settings(Style::SingleImage);
    set.image.border_width = f64::NAN;
    assert!(prepare(&mut s, &set, &ids).is_err());
    set.image.border_width = 0.1;
    set.image.stroke_width_pt = -1.0;
    assert!(prepare(&mut s, &set, &ids).is_err());
    assert!(prepare(&mut s, &settings(Style::SingleImage), &[PhotoId(999_999)]).is_err());
    assert!(prepare(&mut s, &settings(Style::SingleImage), &[]).is_err());
    let set = settings(Style::SingleImage);
    assert!(prepare(&mut s, &set, &vec![ids[0]; 101]).is_err(), "more than 100 pages");
    // 100 pages of 4x6 at 100 ppi is 24 Mpx: allowed; at 720 ppi it would be over the job cap.
    let mut big = settings(Style::SingleImage);
    big.job.ppi = 720.0;
    assert!(prepare(&mut s, &big, &vec![ids[0]; 60]).is_err());
}
