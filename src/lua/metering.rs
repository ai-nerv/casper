//! How long a chunk may run and how much it may hold, for every VM casper builds: the declarations'
//! and the indexer's step through the same loop.

use luna::{Fuel, Lua, StashedExecutor};

/// Step `executor` to the end in slices, stopping it once `budget` instructions are spent.
/// No `__gc` handler runs: the sandbox refuses a metatable carrying one.
pub fn metered(lua: &mut Lua, executor: &StashedExecutor, budget: u64) -> Result<(), String> {
    const SLICE: i32 = 4096;
    let mut spent: u64 = 0;
    loop {
        let mut fuel = Fuel::with(SLICE);
        let finished = lua
            .enter(|ctx| ctx.fetch(executor).step(ctx, &mut fuel))
            .map_err(|why| why.to_string())?;
        spent += u64::try_from(SLICE.saturating_sub(fuel.remaining())).unwrap_or(0);
        if lua.enter(|ctx| ctx.fetch(executor).is_waiting()) {
            lua.enter(|ctx| ctx.fetch(executor).stop(&ctx));
            return Err("a declaration cannot wait on a future here".to_owned());
        }
        if let Some(ceiling) = over_ceiling(lua) {
            lua.enter(|ctx| ctx.fetch(executor).stop(&ctx));
            crate::noted!("lua: stopped over {ceiling} bytes of memory");
            return Err(format!(
                "held more than its {} MiB of memory and was stopped",
                ceiling >> 20
            ));
        }
        if finished {
            return Ok(());
        }
        if spent > budget {
            lua.enter(|ctx| ctx.fetch(executor).stop(&ctx));
            crate::noted!("lua: stopped after {spent} instructions");
            return Err(format!(
                "ran past its budget of {budget} instructions and was stopped"
            ));
        }
    }
}

/// The ceiling, when the VM still holds more than it after a full collection.
fn over_ceiling(lua: &mut Lua) -> Option<usize> {
    let ceiling = lua.memory_limit()?;
    if lua.total_memory() <= ceiling {
        return None;
    }
    lua.gc_collect();
    (lua.total_memory() > ceiling).then_some(ceiling)
}
