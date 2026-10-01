//! Open a COPY of a mind.db the way the Mind does at start -- its engine first, then the Mind's own
//! tables -- and ask the engine to recall from it (E.PRODDB1: is a store safe to move to this build's
//! engine?). Never point this at a live Mind's file: a second engine on one file does not see the
//! first one's writes.
//!
//!     cargo run --release -p mind-memory --example open_db -- /path/to/copy/mind.db [query]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let path = args.next().ok_or("usage: open_db <copy of mind.db> [query]")?;
    let query = args.next().unwrap_or_else(|| "family".to_string());
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let h = mind_memory::MemoryHandle::spawn(&path, 64).map_err(|e| e.to_string())?;
        // One round trip through the memory thread: it finishes its table setup before it reads a
        // command, so an answer here means every migration the Mind runs at start has run.
        let goals = h.list_goals().await.map_err(|e| e.to_string())?;
        println!("opened by the memory layer; goals: {}", goals.len());
        Ok::<_, String>(())
    })?;
    drop(rt);
    std::thread::sleep(std::time::Duration::from_secs(1));
    let db = yantrikdb_core::YantrikDB::new(&path, 64).map_err(|e| e.to_string())?;
    let hits = db.recall_text(&query, 5).map_err(|e| e.to_string())?;
    println!("engine recall {query:?}: {} hit(s)", hits.len());
    Ok(())
}
