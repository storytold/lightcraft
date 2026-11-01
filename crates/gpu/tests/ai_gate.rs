//! Background AI work and the interface (dual-GPU PR 5, decision §8): on the editing card it waits until the interface
//! has been idle a moment; on the second graphics card it never waits. Its own test binary: the interaction tracking is
//! process-wide.

use std::time::{Duration, Instant};

use lightcraft_gpu::Role;
use lightcraft_gpu::ai::background_turn;

#[test]
fn background_ai_work_waits_for_the_interface_only_on_the_editing_card() {
    // not tracked (the CLI, MCP): nothing waits
    let t = Instant::now();
    background_turn(Role::Primary);
    assert!(t.elapsed() < Duration::from_millis(100));

    lightcraft_gpu::track_interaction(true);
    lightcraft_gpu::set_interaction_idle(Duration::from_millis(300));
    lightcraft_gpu::note_interaction();
    let t = Instant::now();
    background_turn(Role::Helper);
    assert!(t.elapsed() < Duration::from_millis(100), "the second card never waits for the interface");
    lightcraft_gpu::note_interaction();
    let t = Instant::now();
    background_turn(Role::Primary);
    let waited = t.elapsed();
    assert!(waited >= Duration::from_millis(200), "the editing card waits for the interface to be idle: {waited:?}");
    assert!(waited < Duration::from_secs(5), "and then goes on: {waited:?}");
    lightcraft_gpu::track_interaction(false);
}
