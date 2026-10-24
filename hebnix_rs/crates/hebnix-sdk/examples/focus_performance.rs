//! Exercise shared UI queries while scrolling, including initial and periodic loads.
use std::time::{Duration, Instant};

fn main() {
    let mut timings = Vec::new();
    let mut binding_timings = Vec::new();
    let mut ready_bindings = 0;
    for _ in 0..480 {
        let start = Instant::now();
        std::hint::black_box(hebnix_sdk::process::is_rocket_league_focused());
        timings.push(start.elapsed());
        let start = Instant::now();
        let bindings = std::hint::black_box(hebnix_sdk::input::action_binds("scoreboard"));
        binding_timings.push(start.elapsed());
        ready_bindings += usize::from(!bindings.is_empty());
        std::thread::sleep(Duration::from_millis(16));
    }
    timings.sort();
    println!(
        "Focus query: median {:?}, p99 {:?}, maximum {:?} ({} samples)",
        timings[timings.len() / 2],
        timings[timings.len() * 99 / 100],
        timings.last().unwrap(),
        timings.len(),
    );
    binding_timings.sort();
    println!(
        "Binding query: median {:?}, p99 {:?}, maximum {:?}",
        binding_timings[binding_timings.len() / 2],
        binding_timings[binding_timings.len() * 99 / 100],
        binding_timings.last().unwrap()
    );
    println!("Nonempty scoreboard binding snapshots: {ready_bindings}/480");
}
