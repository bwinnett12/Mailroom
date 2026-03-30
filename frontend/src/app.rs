use leptos::*;
use crate::components::record_list::RecordList;

#[component]
pub fn App() -> impl IntoView {
    // Explicitly return the view to help inference
    let content = view! {
        <main class="p-6 bg-slate-950 min-h-screen text-slate-200">
            <div class="max-w-4xl mx-auto">
                <h1 class="text-3xl font-mono text-emerald-500 mb-8">
                    "Island // Mailroom"
                </h1>
                <RecordList />
            </div>
        </main>
    };
    content
}