use leptos::*;
use frontend::App; // This assumes your main component is in lib.rs

fn main() {
    // This connects the Rust WASM to the <body> of your index.html
    mount_to_body(|| view! { <App /> })
}