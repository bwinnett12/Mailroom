use leptos::*;
use crate::components::record_list::RecordList;

#[component]
pub fn App() -> impl IntoView {
    view! {
        <main class="p-6 bg-slate-950 min-h-screen text-slate-200">
            <div class="max-w-4xl mx-auto">
                <h1 class="text-3xl font-mono text-emerald-500 mb-8">
                    "Island // Mailroom"
                </h1>
                
                // This component is in frontend/src/components/record_list.rs
                <RecordList />
            </div>
        </main>
    }
}