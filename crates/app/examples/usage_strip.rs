//! Fetches the real usage numbers once and prints the strip as text at a few widths.
//!   cargo run -p bakedit --example usage_strip

use bakedit::ui::Canvas;
use bakedit::usage;
use bakedit_core::Theme;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

fn main() {
    let vendors = usage::fetch();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    let theme = Theme::builtin();
    for width in [160u16, 100, 60, 40] {
        let area = Rect::new(0, 0, width, 1);
        let mut buf = Buffer::empty(area);
        {
            let mut canvas = Canvas::new(&mut buf, area);
            usage::render(&mut canvas, area, &vendors, now, &theme);
        }
        println!("{width:>3}|{}|", bakedit::dump::to_text(&buf).trim_end_matches('\n'));
    }
}
