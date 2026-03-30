use leptos::*;
use crate::components::record_list::RecordList;

#[component]
pub fn App() -> impl IntoView {
    // Explicitly returning the view helps the Rust compiler 
    // infer that this is a Leptos Element.
    let view = view! {
        <div class="app-container">
            <main class="p-6 bg-slate-950 min-h-screen text-slate-200">
                <div class="max-w-4xl mx-auto">
                    <h1 class="text-3xl font-mono text-emerald-500 mb-8">
                        "Island // Mailroom"
                    </h1>
                    <RecordList />
                </div>
            </main>
        </div>
    };
    view
}