# Step 1 wiring: changes to existing files

New files (copy as-is):
- `routes.toml`      → `$MAILROOM_VAULT/routes.toml`
- `routing.rs`       → `src/routing.rs`
- `dispatch.rs`      → `src/routes/dispatch.rs`

Edits to existing files:

## src/route.rs

Add a variant to `Hop`, after `Store`:

```rust
    /// Append one JSON line of envelope metadata (no payload) to a file
    /// under the vault. Path is relative to vault_root. With no envelope,
    /// the line records only the nest and the time.
    Log { path: std::path::PathBuf },
```

Add an arm to the `match hop` in `run_stop`, after the `Hop::Store` arm:

```rust
            Hop::Log { path } => {
                let full = vault_root.join(path);
                if let Some(parent) = full.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let line = serde_json::json!({
                    "at": chrono::Utc::now(),
                    "nest": current_nest_id,
                    "envelope": envelope.map(|e| serde_json::json!({
                        "id": e.id,
                        "data_type": e.data_type,
                        "source": e.source,
                        "jd_address": e.jd_address,
                        "created_at": e.created_at,
                        "received_at": e.received_at,
                        "tags": e.tags,
                        "meta": e.meta,
                    })),
                });
                use std::io::Write;
                let mut file = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&full)?;
                writeln!(file, "{line}")?;
            }
```

## src/state.rs

Add a field to `AppState`:

```rust
    /// Config-driven routing table, loaded from routes.toml at startup.
    pub router: std::sync::Arc<crate::routing::RoutingTable>,
```

Add a parameter to `AppState::new` (`router: crate::routing::RoutingTable`) and set:

```rust
            router: std::sync::Arc::new(router),
```

## src/main.rs

Add the module declaration with the others:

```rust
mod routing;
```

After the registry is built, and before `AppState::new` (which moves `vault_path`):

```rust
    let routing = routing::RoutingTable::load(
        &vault_path.join("routes.toml"),
        &vault_path,
        &registry,
    )?;
    tracing::info!(routes = routing.route_count(), "routing table ready");
```

Pass `routing` as the new last argument to `AppState::new(...)`.

`route_count()` is defined on `RoutingTable` in `routing.rs`.

## src/routes/mod.rs

Add the module:

```rust
pub mod dispatch;
```

Add the route, next to the envelope routes. Use `/dispatch`, not `/envelope`,
because `/envelope` already belongs to `envelope::receive`:

```rust
.route("/dispatch", post(dispatch::dispatch))
```

## Notes

- Both `RoutingTable::load` and `Dodo` are `Send + Sync`, so `Arc<AppState>`
  stays valid across the handler's `.await`.
- `Route::run` already resolves `Hop::Store` through
  `move_branch::find_nest_by_id`. The table's `nest` value must match a nest
  id, which startup validation guarantees.
- Routes resolve the store target with `vault_root` as `library_root`. That's
  the same caveat `route.rs` already documents.
