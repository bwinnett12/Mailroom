use leptos::prelude::*;
use leptos::task::spawn_local;
use wasm_bindgen::prelude::*;
use gloo_net::http::Request;
use serde::{Deserialize, Serialize};
use log::{info, error};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RegistryData {
    pub nodes: Vec<serde_json::Value>,
    pub count: usize,
}

#[component]
fn App() -> impl IntoView {
    let (registry, set_registry) = signal::<Option<RegistryData>>(None);
    let (selected_node, set_selected_node) = signal::<Option<serde_json::Value>>(None);

    let (node_details, set_node_details) = signal::<Option<serde_json::Value>>(None);

    // In the click handler:
    let node_id = node_id_clone.clone();
    spawn_local(async move {
        if let Ok(resp) = Request::get(&format!("/api/registry/{}", node_id)).send().await {
            if let Ok(data) = resp.json::<serde_json::Value>().await {
                set_node_details.set(Some(data));
            }
        }
    });

    Effect::new(move |_| {
        info!("App mounted, fetching registry...");
        spawn_local(async move {
            match Request::get("/api/registry/graph").send().await {
                Ok(resp) => {
                    info!("Got response: {:?}", resp.status());
                    match resp.json::<RegistryData>().await {
                        Ok(data) => {
                            info!("Parsed registry: {} nodes", data.count);
                            set_registry.set(Some(data));
                        }
                        Err(e) => error!("JSON parse error: {:?}", e),
                    }
                }
                Err(e) => error!("Request error: {:?}", e),
            }
        });
    });

    view! {
        <div class="app-container">
            <header>
                <h1>"Mailroom"</h1>
                <p>"Personal knowledge routing & graph visualization"</p>
            </header>
            
            <div class="graph-and-sidebar">
                <main class="graph-section">
                    <h2>"JD Graph"</h2>
                    <Suspense fallback=move || view! { <p>"Graph component loading..."</p> }>
                        {move || {
                            registry.get().map(|reg| {
                                info!("Rendering graph with {} nodes", reg.count);
                                view! {
                                    <svg class="graph-svg" viewBox="0 0 1000 600" width="100%" height="600">
                                        {reg.nodes.iter().enumerate().map(|(i, node)| {
                                            let node_id = node
                                                .get("id")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or(&format!("Node {}", i))
                                                .to_string();
                                            
                                            let node_kind = node
                                                .get("kind")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("unknown");
                                            
                                            let fill_color = match node_kind {
                                                "leaf" => "#b3e5fc",
                                                "area" => "#fff9c4",
                                                _ => "#e8f4f8",
                                            };
                                            
                                            let x = 50.0 + ((i as f32) % 5.0) * 150.0;
                                            let y = 100.0 + (((i as f32) / 5.0).floor() * 100.0);
                                            
                                            let node_clone = node.clone();
                                            let node_id_clone = node_id.clone();
                                            
                                            view! {
                                                <g
                                                    style="cursor: pointer"
                                                    on:click=move |_| {
                                                        info!("Clicked node: {}", node_id_clone);
                                                        set_selected_node.set(Some(node_clone.clone()));
                                                    }
                                                >
                                                    <rect 
                                                        x={x.to_string()} 
                                                        y={y.to_string()} 
                                                        width="120" 
                                                        height="60" 
                                                        fill={fill_color}
                                                        stroke="#333" 
                                                        stroke-width="2" 
                                                    />
                                                    <text 
                                                        x={(x + 60.0).to_string()} 
                                                        y={(y + 35.0).to_string()} 
                                                        text-anchor="middle" 
                                                        font-size="12"
                                                        font-weight="bold"
                                                    >
                                                        {node_id}
                                                    </text>
                                                </g>
                                            }
                                        }).collect::<Vec<_>>()}
                                    </svg>
                                }
                            })
                        }}
                    </Suspense>
                </main>

                <aside class="sidebar">
                    <h3>"Node Details"</h3>
                    {move || {
                        selected_node.get().map(|node| {
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
                        })
                    }}
                </aside>
            </div>
        </div>
    }
}

#[wasm_bindgen(start)]
pub fn main() {
    console_log::init_with_level(log::Level::Info).unwrap();
    info!("Mailroom WASM app starting...");
    leptos::mount::mount_to_body(|| view! { <App /> });
}

// Pseudocode for tree component
#[component]
fn TreeNode(node: Value, children: Vec<Value>) -> impl IntoView {
    let (expanded, set_expanded) = signal(false);
    
    view! {
        <div class="tree-node">
            <button on:click=move |_| set_expanded(!expanded.get())>
                {if expanded.get() { "▼" } else { "▶" }}
            </button>
            <span on:click=move |_| set_selected_node(node.clone())>
                {node.get("name")}
            </span>
            
            {move || if expanded.get() {
                view! {
                    <div class="tree-children">
                        {children.iter().map(|child| {
                            view! { <TreeNode node={child.clone()} children={vec![]} /> }
                        }).collect::<Vec<_>>()}
                    </div>
                }
            }}
        </div>
    }
}