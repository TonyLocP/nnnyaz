use std::io;

mod app;
mod preview;
mod util;

fn main() -> io::Result<()> {
    let mut app = app::App::new()?;
    let mut terminal = ratatui::init();
    let res = app.run(&mut terminal);
    ratatui::restore();
    res
}