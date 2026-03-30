use frontend::app::App; // Adjust this if your App component is elsewhere
use leptos::*;
use app::App;

fn main() {
    // Optional: add logging to help debug in the browser console
    _ = console_log::init_with_level(log::Level::Debug);
    console_error_panic_hook::set_once();

    mount_to_body(|| {
        view! { <App /> }
    });
}