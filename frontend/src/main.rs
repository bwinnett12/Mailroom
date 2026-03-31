use leptos::*;
use frontend::app::App; // This should work once lib.rs is pub

fn main() {
    // Add these dependencies to frontend/Cargo.toml if they are missing:
    // console_log = "1.0"
    // console_error_panic_hook = "0.1"
    // log = "0.4"
    
    _ = console_log::init_with_level(log::Level::Debug);
    console_error_panic_hook::set_once();

    mount_to_body(|| view! { <App /> });
}