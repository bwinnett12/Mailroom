// Placeholder for SSR integration
// For now, the Mailroom backend (Axum in mailroom/) will serve the static WASM assets
// This file exists as a template for future SSR features

fn main() {
    eprintln!("Note: mailroom-ui is compiled to WASM and served statically by the Mailroom backend.");
    eprintln!("Run 'cargo build --target wasm32-unknown-unknown' to build the WASM frontend.");
}
