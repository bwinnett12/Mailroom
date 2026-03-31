use leptos::*;
use leptos_router::*;
pub use common::DecimalRecord;
pub mod app;
pub mod components;

#[component]
pub fn App() -> impl IntoView {
    view! {
        <Router>
            <main class="p-6 max-w-4xl mx-auto">
                <h1 class="text-3xl font-bold border-b pb-2 mb-4">"Mailroom: Johnny.Decimal"</h1>
                <Routes>
                    <Route path="" view=|| view! { <Dashboard /> } />
                </Routes>
            </main>
        </Router>
    }
}

#[component]
fn Dashboard() -> impl IntoView {
    view! {
        <div class="grid gap-6">
            <p class="text-gray-600">"Welcome to your decentralized brain."</p>
            // We will drop the RecordList and LivePreview here next
        </div>
    }
}