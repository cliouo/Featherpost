#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "macos")]
fn main() {
    macos::run();
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("{}: unsupported platform", maild_brand::MENU_BIN);
    std::process::exit(1);
}
