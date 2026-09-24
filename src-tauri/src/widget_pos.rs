use crate::state::SharedState;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::Duration;

#[derive(Serialize, Deserialize)]
struct StoredPos {
    x: i32,
    y: i32,
}

pub fn load(path: &Path) -> Option<(i32, i32)> {
    let content = std::fs::read_to_string(path).ok()?;
    let pos: StoredPos = serde_json::from_str(&content).ok()?;
    Some((pos.x, pos.y))
}

fn save(path: &Path, x: i32, y: i32) {
    let json = match serde_json::to_string(&StoredPos { x, y }) {
        Ok(json) => json,
        Err(_) => return,
    };
    if let Err(err) = crate::atomic_io::write_durable(path, json.as_bytes()) {
        tracing::warn!("widget position save failed: {err}");
    }
}

pub fn flush(state: &SharedState) {
    if state.widget_move_gen.load(Ordering::Relaxed) > 0 {
        save(
            &state.widget_pos_path,
            state.widget_x.load(Ordering::Relaxed),
            state.widget_y.load(Ordering::Relaxed),
        );
    }
}

pub fn record_move(state: &SharedState, x: i32, y: i32) {
    state.widget_x.store(x, Ordering::Relaxed);
    state.widget_y.store(y, Ordering::Relaxed);
    let generation = state.widget_move_gen.fetch_add(1, Ordering::Relaxed) + 1;
    let state = state.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_millis(500)).await;
        if state.widget_move_gen.load(Ordering::Relaxed) == generation {
            save(
                &state.widget_pos_path,
                state.widget_x.load(Ordering::Relaxed),
                state.widget_y.load(Ordering::Relaxed),
            );
        }
    });
}
