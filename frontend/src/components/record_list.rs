use leptos::*;

// 1. Define the fetching function (Stub for now)
async fn fetch_records() -> Vec<String> {
    vec!["Johnny.Decimal 10-19".to_string(), "Johnny.Decimal 20-29".to_string()]
}

#[component]
pub fn RecordList() -> impl IntoView {
    // 2. Explicitly type the resource so the view knows what 'data' is
    let records = create_resource(|| (), |_| async move { fetch_records().await });

    view! {
        <div class="space-y-4">
            <Transition fallback=move || view! { <p>"Loading Island data..."</p> }>
                {move || {
                    records.get().map(|data: Vec<String>| { // Type hint added here
                        data.into_iter()
                            .map(|rec| view! { 
                                <div class="p-2 border border-slate-800 rounded">
                                    {rec}
                                </div> 
                            })
                            .collect_view()
                    })
                }}
            </Transition>
        </div>
    }
}