use leptos::prelude::*;
use leptos::task::spawn_local;
use wasm_bindgen::prelude::*;
use gloo_net::http::Request;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RegistryData {
    pub nodes: Vec<serde_json::Value>,
    pub count: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FilesResponse {
    pub files: Vec<String>,
}

#[derive(Clone, Copy, PartialEq)]
enum ViewTab {
    Graph,
    List,
}



#[component]
fn App() -> impl IntoView {
    let (registry, set_registry) = signal::<Option<RegistryData>>(None);
    let (selected_node, set_selected_node) = signal::<Option<serde_json::Value>>(None);
    let (current_file, set_current_file) = signal::<Option<String>>(None);
    let (file_content, set_file_content) = signal::<Option<String>>(None);
    let (files_list, set_files_list) = signal::<Vec<String>>(Vec::new());
    let (view_tab, set_view_tab) = signal::<ViewTab>(ViewTab::List);
    let (search_query, _set_search_query) = signal::<String>(String::new());
    let (has_main_file, set_has_main_file) = signal::<std::collections::HashSet<String>>(std::collections::HashSet::new());

    pub struct EnvelopeRoute {
        pub data_type: String,
        pub dest_node_id: String,
        pub hops: Vec<Hop>,
        pub dodo_id: String,
    }

    // In config or registry
    let routes = vec![
        EnvelopeRoute {
            data_type: "text/todo".to_string(),
            dest_node_id: "22".to_string(),
            hops: vec![
                Hop::RunScript { ... }, // format-todo
                Hop::Store,
            ],
            dodo_id: "upkeep-manager".to_string(),
        },
        EnvelopeRoute {
            data_type: "text/calendar-event".to_string(),
            dest_node_id: "20-C".to_string(),
            hops: vec![
                Hop::RunScript { ... }, // format-event
                Hop::Store,
            ],
            dodo_id: "scheduler".to_string(),
        },
        EnvelopeRoute {
            data_type: "text/log".to_string(),
            dest_node_id: "88".to_string(),
            hops: vec![
                Hop::Store, // no formatting, raw
            ],
            dodo_id: "logger".to_string(),
        },
    ];

    // Handler
    pub async fn handle_envelope(
        State(state): State<Arc<AppState>>,
        payload: InboundEnvelope,
    ) -> impl IntoResponse {
        let envelope = payload.into_envelope();

        // Look up routing
        let route_config = state.routes.iter()
            .find(|r| r.data_type == envelope.data_type)?;

        // Build the Route from config
        let mut route = Route::new(
            route_config.dodo_id.clone(),
            vec![Stop {
                nest_id: route_config.dest_node_id.clone(),
                hops: route_config.hops.clone(),
            }],
        );

        // Execute via Dodo (authorization built-in)
        let dodo = state.dodos.get(&route_config.dodo_id).await?;
        dodo.run_route(&mut route, &state.vault_root, Some(&envelope)).await?;

        Ok((
            StatusCode::CREATED,
            Json(json!({
                "envelope_id": envelope.id,
                "routed_to": route_config.dest_node_id,
                "route_status": route.status,
            })),
        ))
    }

    // Fetch registry on mount
    Effect::new(move |_| {
        spawn_local(async move {
            match Request::get("/api/registry/graph").send().await {
                Ok(resp) => {
                    match resp.json::<RegistryData>().await {
                        Ok(data) => { 
                            // Auto-load node 00 before moving data
                            if let Some(node_00) = data.nodes.iter().find(|n| 
                                n.get("id").and_then(|v| v.as_str()) == Some("00")
                            ) {
                                set_selected_node.set(Some(node_00.clone()));
                            }
                            set_registry.set(Some(data));
                        }
                        Err(e) => leptos::logging::error!("JSON parse error: {:?}", e),
                    }
                }
                Err(e) => leptos::logging::error!("Request error: {:?}", e),
            }
        });
    });

    // Check which nodes have .00_ files
    Effect::new(move |_| {
        if let Some(reg) = registry.get() {
            for node in &reg.nodes {
                if let Some(node_id) = node.get("id").and_then(|v| v.as_str()) {
                    let id = node_id.to_string();
                    spawn_local({
                        let node_id_str = id.clone();
                        async move {
                            match Request::get(&format!("/api/registry/{}/files", node_id_str))
                                .send()
                                .await
                            {
                                Ok(resp) => {
                                    if let Ok(FilesResponse { files }) = resp.json().await {
                                        if files.iter().any(|f| f.starts_with(&format!("{}.00_", node_id_str))) {
                                            set_has_main_file.update(|m| { m.insert(node_id_str); });
                                        }
                                    }
                                }
                                Err(_) => {}
                            }
                        }
                    });
                }
            }
        }
    });

    // Fetch files when node is selected
    Effect::new(move |_| {
        if let Some(node) = selected_node.get() {
            if let Some(node_id) = node.get("id").and_then(|v| v.as_str()) {
                let node_id_str = node_id.to_string();

                spawn_local({
                    let id = node_id_str.clone();
                    async move {
                        match Request::get(&format!("/api/registry/{}/files", id))
                            .send()
                            .await
                        {
                            Ok(resp) => {
                                match resp.json::<FilesResponse>().await {
                                    Ok(data) => {
                                        set_files_list.set(data.files.clone());
                                        
                                        if let Some(file) = find_best_readme(&id, &data.files) {
                                            set_current_file.set(Some(file.clone()));
                                        } else if !data.files.is_empty() {
                                            set_current_file.set(Some(data.files[0].clone()));
                                        }
                                    }
                                    Err(e) => leptos::logging::error!("Files parse error: {:?}", e),
                                }
                            }
                            Err(e) => leptos::logging::error!("Files fetch error: {:?}", e),
                        }
                    }
                });
            }
        }
    });

    // Fetch file content when current file changes
    Effect::new(move |_| {
        if let Some(node) = selected_node.get() {
            if let Some(node_id) = node.get("id").and_then(|v| v.as_str()) {
                if let Some(filename) = current_file.get() {
                    let node_id_str = node_id.to_string();
                    spawn_local({
                        let id = node_id_str.clone();
                        let file = filename.clone();
                        async move {
                            let url = format!("/api/registry/{}/files/{}", id, file);
                            match Request::get(&url).send().await {
                                Ok(resp) => {
                                    match resp.text().await {
                                        Ok(content) => { set_file_content.set(Some(content)); }
                                        Err(e) => leptos::logging::error!("File read error: {:?}", e),
                                    }
                                }
                                Err(e) => leptos::logging::error!("File fetch error: {:?}", e),
                            }
                        }
                    });
                }
            }
        }
    });

    view! {
        <div class="app-container">
            <header>
                <h1>"Mailroom"</h1>
                <p>"Personal knowledge routing & graph visualization"</p>
            </header>

            <div class="main-content">
                <section class="top-section">
                    <div class="tabs">
                        <button
                            class=move || if view_tab.get() == ViewTab::Graph { "tab active" } else { "tab" }
                            on:click=move |_| set_view_tab.set(ViewTab::Graph)
                        >
                            "Graph"
                        </button>
                        <button
                            class=move || if view_tab.get() == ViewTab::List { "tab active" } else { "tab" }
                            on:click=move |_| set_view_tab.set(ViewTab::List)
                        >
                            "Index"
                        </button>
                    </div>

                    <Show when=move || view_tab.get() == ViewTab::Graph>
                        <div class="graph-container">
                            <Suspense fallback=move || view! { <p>"Loading graph..."</p> }>
                                {move || registry.get().map(|reg| {
                                    let query = search_query.get().to_lowercase();
                                    let filtered: Vec<_> = reg.nodes.iter()
                                        .filter(|node| {
                                            let id = node.get("id").and_then(|v| v.as_str()).unwrap_or("");
                                            !id.contains('-') && !id.contains('.')
                                        })
                                        .filter(|node| {
                                            let id = node.get("id").and_then(|v| v.as_str()).unwrap_or("").to_lowercase();
                                            let name = node.get("name").and_then(|v| v.as_str()).unwrap_or("").to_lowercase();
                                            query.is_empty() || id.contains(&query) || name.contains(&query)
                                        })
                                        .collect();
                                    
                                    view! {
                                        <div class="nodes-grid">
                                            {filtered.into_iter().map(|node| {
                                                let node_id = node.get("id").and_then(|v| v.as_str()).unwrap_or("—").to_string();
                                                let node_name = node.get("name").and_then(|v| v.as_str()).unwrap_or("—").to_string();
                                                let node_kind = node.get("kind").and_then(|v| v.as_str()).unwrap_or("unknown");
                                                let has_file = has_main_file.get().contains(&node_id);
                                                
                                                let bg_color = match node_kind {
                                                    "leaf" => "#b3e5fc",
                                                    "area" => "#fff9c4",
                                                    _ => "#e8f4f8",
                                                };
                                                
                                                let opacity = if has_file { "1" } else { "0.4" };
                                                let node_clone = node.clone();
                                                view! {
                                                    <div
                                                        class="node-item"
                                                        style=format!("background-color: {}; opacity: {};", bg_color, opacity)
                                                        on:click=move |_| { 
                                                            set_selected_node.set(Some(node_clone.clone()));
                                                            set_current_file.set(None);
                                                            set_file_content.set(None);
                                                        }
                                                    >
                                                        <div class="node-id">{node_id}</div>
                                                        <div class="node-name">{node_name}</div>
                                                    </div>
                                                }
                                            }).collect::<Vec<_>>()}
                                        </div>
                                    }
                                })}
                            </Suspense>
                        </div>
                    </Show>

                    <Show when=move || view_tab.get() == ViewTab::List>
                        <div class="list-container">
                            <Suspense fallback=move || view! { <p>"Loading index..."</p> }>
                                {move || registry.get().map(|reg| {
                                    let mut nodes_by_tens: std::collections::BTreeMap<u32, Vec<_>> = std::collections::BTreeMap::new();
                                    
                                    for node in &reg.nodes {
                                        if let Some(id) = node.get("id").and_then(|v| v.as_str()) {
                                            if !id.contains('-') && !id.contains('.') {
                                                if let Ok(num) = id.parse::<u32>() {
                                                    let tens = (num / 10) * 10;
                                                    nodes_by_tens.entry(tens).or_insert_with(Vec::new).push((id.to_string(), node.clone()));
                                                }
                                            }
                                        }
                                    }
                                    
                                    view! {
                                        <div class="index-table">
                                            {nodes_by_tens.into_iter().map(|(tens, mut nodes)| {
                                                nodes.sort_by(|a, b| a.0.cmp(&b.0));
                                                let section_label = format!("{:02}-{:02}", tens, tens + 9);
                                                view! {
                                                    <div class="index-section">
                                                        <h3 class="section-header">{section_label}</h3>
                                                        <div class="section-grid">
                                                            {nodes.into_iter().map(|(node_id, node)| {
                                                                let node_name = node.get("name").and_then(|v| v.as_str()).unwrap_or("—").to_string();
                                                                let has_file = has_main_file.get().contains(&node_id);
                                                                let opacity = if has_file { "1" } else { "0.5" };
                                                                let cursor = if has_file { "pointer" } else { "not-allowed" };
                                                                let node_clone = node.clone();
                                                                
                                                                view! {
                                                                    <div
                                                                        class="index-cell"
                                                                        style=format!("opacity: {}; cursor: {};", opacity, cursor)
                                                                        on:click=move |_| {
                                                                            if has_file {
                                                                                set_selected_node.set(Some(node_clone.clone()));
                                                                                set_current_file.set(None);
                                                                                set_file_content.set(None);
                                                                            }
                                                                        }
                                                                    >
                                                                        <div class="cell-id">{node_id}</div>
                                                                        <div class="cell-name">{node_name}</div>
                                                                    </div>
                                                                }
                                                            }).collect::<Vec<_>>()}
                                                        </div>
                                                    </div>
                                                }
                                            }).collect::<Vec<_>>()}
                                        </div>
                                    }
                                })}
                            </Suspense>
                        </div>
                    </Show>

                    <aside class="sidebar">
                        <h3>"Node Details"</h3>
                        {move || selected_node.get().map(|node| {
                            let id = node.get("id").and_then(|v| v.as_str()).unwrap_or("—").to_string();
                            let name = node.get("name").and_then(|v| v.as_str()).unwrap_or("—").to_string();
                            let kind = node.get("kind").and_then(|v| v.as_str()).unwrap_or("—").to_string();
                            let path = node.get("path").and_then(|v| v.as_str()).unwrap_or("—").to_string();
                            let store = node.get("store").and_then(|v| v.as_str()).unwrap_or("—").to_string();
                            view! {
                                <div class="node-details">
                                    <p><strong>"ID: "</strong>{id}</p>
                                    <p><strong>"Name: "</strong>{name}</p>
                                    <p><strong>"Kind: "</strong>{kind}</p>
                                    <p><strong>"Path: "</strong>{path}</p>
                                    <p><strong>"Store: "</strong>{store}</p>
                                </div>
                            }
                        })}
                    </aside>
                </section>

                // HTMX form (same root endpoint)
                <form hx-post="/envelope" hx-target="#result">
                    <input name="data_type" value="text/calendar-event" type="hidden" />
                    <input name="title" placeholder="Event name" required />
                    <input name="start_time" type="datetime-local" required />
                    <input name="end_time" type="datetime-local" />
                    <input name="location" placeholder="Where?" />
                    <select name="ref">
                        <option value="">— Not linked</option>
                        <option value="35_Health">35 Health</option>
                        <option value="20-A">20-A At-a-glance</option>
                    </select>
                    <button type="submit">Add Event</button>
                </form>

                <section class="bottom-section">
                    <Show when=move || selected_node.get().is_some()>
                        <div class="file-selector">
                            <select on:change=move |ev| {
                                let filename = event_target_value(&ev);
                                if !filename.is_empty() {
                                    set_current_file.set(Some(filename));
                                }
                            }>
                                <option value="">"Select a file..."</option>
                                {move || files_list.get().into_iter().map(|file| {
                                    let file_display = file.clone();
                                    view! {
                                        <option value=file>{file_display}</option>
                                    }
                                }).collect::<Vec<_>>()}
                            </select>
                        </div>
                        
                        <Show when=move || file_content.get().is_some()>
                            <div class="file-viewer">
                                <h3>{move || current_file.get().unwrap_or_default()}</h3>
                                <div class="file-content">
                                    {move || file_content.get().unwrap_or_default()}
                                </div>
                            </div>
                        </Show>
                    </Show>
                </section>
            </div>
        </div>
    }
}

fn find_best_readme(node_id: &str, files: &[String]) -> Option<String> {
    if let Some(file) = files.iter().find(|f| f.starts_with(&format!("{}.00_", node_id)) && f.ends_with(".md")) {
        return Some(file.clone());
    }
    if let Some(file) = files.iter().find(|f| f.as_str() == format!("{}.md", node_id).as_str()) {
        return Some(file.clone());
    }
    if let Some(file) = files.iter().find(|f| f.as_str() == "README.md") {
        return Some(file.clone());
    }
    if let Some(file) = files.iter().find(|f| f.ends_with(".md")) {
        return Some(file.clone());
    }
    None
}

#[wasm_bindgen(start)]
pub fn main() {
    console_log::init_with_level(log::Level::Info).unwrap();
    leptos::mount::mount_to_body(|| view! { <App /> });
}