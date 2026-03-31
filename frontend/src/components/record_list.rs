use leptos::*;

#[component]
pub fn RecordList() -> impl IntoView {
    // 1. Define the resource
    // The first argument is the "source" (usually a signal like a search bar). 
    // If it's (), it just runs once on load.
    let records = create_resource(|| (), |_| async move { fetch_records().await });

    view! {
        <div class="p-4">
            <h1 class="text-xl font-bold">"Johnny.Decimal Index"</h1>
            
            // 2. Use a Transition or Suspense component to handle the loading state
            <Transition fallback=move || view! { <p>"Loading your brain..."</p> }>
                {move || {
                    records.get().map(|data| {
                        view! {
                            <ul class="space-y-2 mt-4">
                                {data.into_iter().map(|rec| {
                                    view! {
                                        <li class="border-l-4 border-blue-500 pl-3">
                                            <span class="font-mono text-blue-600">{rec.code}</span>
                                            <span class="ml-2 font-semibold">{rec.title}</span>
                                        </li>
                                    }
                                }).collect_view()}
                            </ul>
                        }
                    })
                }}
            </Transition>
        </div>
    }
}