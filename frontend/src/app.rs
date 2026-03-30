use leptos::*;
use crate::components::record_list::RecordList; // Make sure components are in frontend/src/

#[component]
pub fn App() -> impl IntoView {
    view! {
        <main class="p-4">
            <h1 class="text-2xl font-bold">"Mailroom Dashboard"</h1>
            <RecordList />
        </main>
    }
}