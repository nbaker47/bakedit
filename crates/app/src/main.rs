mod cli;
mod format;
mod ipc;
mod layout;
mod line_input;
mod picker;
mod session;
mod settings;
mod ui;
mod workspace;

fn main() {
    println!("bakedit {}", env!("CARGO_PKG_VERSION"));
}
