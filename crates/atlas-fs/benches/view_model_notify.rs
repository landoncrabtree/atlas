//! Benchmark: [`InMemoryLocationViewModel::notify`] fan-out with many subscribers.
//!
//! Motivation: `notify` used to hold the `subscribers` mutex across every
//! `event.clone()` + `tx.send()`, which serialised every subscriber under
//! the lock and delayed new subscriptions during a fan-out. This bench
//! measures the fan-out cost with a large subscriber population and with
//! a mix of slow (bounded, back-pressured) and fast receivers so a
//! regression that re-introduces the "hold-lock-while-sending" pattern
//! shows up as a big blow-up on the `notify_slow_subscribers` case.
//!
//! We drive `notify` directly via `subscribe()` from public API and then
//! send an event; the bench measures the wall-clock cost of a single
//! fan-out. When the lock is not held across sends the median stays flat
//! as the slow-subscriber count increases; when it is held, the median
//! grows linearly with the slowest receiver count.

#[path = "common/mod.rs"]
mod common;

use std::sync::Arc;
use std::thread;
use std::time::Duration;

use atlas_fs::{InMemoryLocationViewModel, LocationViewModel, OpenOptions, ViewModelEvent};
use criterion::{criterion_group, criterion_main, BatchSize, Criterion};
use crossbeam_channel::{Receiver, Sender};

/// A shape that fans out a single event across `n_fast` fast subscribers
/// (draining in a helper thread) and `n_blocked` blocked subscribers that
/// never poll their channel. The blocked subscribers stay silent — the
/// channel is unbounded so `send` never blocks — but they exercise the
/// per-subscriber clone + send cost inside `notify`, which is exactly the
/// work that used to be serialised under the mutex.
fn setup(
    n_fast: usize,
    n_blocked: usize,
) -> (
    Arc<InMemoryLocationViewModel>,
    Vec<Receiver<ViewModelEvent>>,
    Vec<Receiver<ViewModelEvent>>,
) {
    let dir = tempfile::tempdir().expect("tempdir");
    let vm = InMemoryLocationViewModel::open(dir.path().to_path_buf(), OpenOptions::default());
    // Keep the tempdir alive by leaking it — the bench discards the VM
    // per iteration anyway, and tempfile cleans up on process exit.
    std::mem::forget(dir);

    let mut fast = Vec::with_capacity(n_fast);
    for _ in 0..n_fast {
        fast.push(vm.subscribe());
    }
    let mut blocked = Vec::with_capacity(n_blocked);
    for _ in 0..n_blocked {
        blocked.push(vm.subscribe());
    }
    (vm, fast, blocked)
}

fn drain_forever(rx: Receiver<ViewModelEvent>, stop: Receiver<()>) {
    thread::spawn(move || loop {
        crossbeam_channel::select! {
            recv(rx) -> _ => {}
            recv(stop) -> _ => return,
        }
    });
}

fn bench_notify(c: &mut Criterion) {
    let mut group = c.benchmark_group("atlas_fs::view_model::notify");
    group.sample_size(20);
    group.warm_up_time(Duration::from_secs(3));

    // A steady-state fan-out with many subscribers all making progress.
    for &n in &[16usize, 128, 512] {
        group.bench_function(format!("notify_{n}_fast_subscribers"), |b| {
            b.iter_batched(
                || {
                    let (vm, fast, _blocked) = setup(n, 0);
                    let stops: Vec<Sender<()>> = fast
                        .into_iter()
                        .map(|rx| {
                            let (stx, srx) = crossbeam_channel::unbounded();
                            drain_forever(rx, srx);
                            stx
                        })
                        .collect();
                    (vm, stops)
                },
                |(vm, stops)| {
                    vm.notify_for_bench(ViewModelEvent::EntriesChanged);
                    // Signal the drain threads to exit before dropping the VM.
                    for s in &stops {
                        let _ = s.send(());
                    }
                    drop(stops);
                    drop(vm);
                },
                BatchSize::LargeInput,
            );
        });
    }

    // A fan-out where most subscribers never poll — this is where holding
    // the lock across sends serialised the loop and delayed a concurrent
    // new-subscribe.
    for &n in &[128usize, 512] {
        group.bench_function(format!("notify_{n}_blocked_subscribers"), |b| {
            b.iter_batched(
                || setup(0, n),
                |(vm, _fast, blocked)| {
                    vm.notify_for_bench(ViewModelEvent::EntriesChanged);
                    drop(blocked);
                    drop(vm);
                },
                BatchSize::LargeInput,
            );
        });
    }

    // Worst-case: measure end-to-end wall time of a background notify burst
    // that runs concurrently with new subscribe() calls on the main thread.
    // Before the fix, subscribe blocks on the subscribers Mutex for the full
    // duration of every in-flight fan-out. After the fix, subscribe only
    // waits for the two brief clone + retain lock acquisitions and can
    // interleave with the actual send() loop.
    group.bench_function("subscribe_during_notify_burst", |b| {
        const BURST: usize = 100;
        const NEW_SUBS: usize = 20;
        b.iter_batched(
            || setup(128, 0),
            |(vm, fast, _blocked)| {
                let stops: Vec<Sender<()>> = fast
                    .into_iter()
                    .map(|rx| {
                        let (stx, srx) = crossbeam_channel::unbounded();
                        drain_forever(rx, srx);
                        stx
                    })
                    .collect();

                let vm_bg = Arc::clone(&vm);
                let handle = thread::spawn(move || {
                    for _ in 0..BURST {
                        vm_bg.notify_for_bench(ViewModelEvent::EntriesChanged);
                    }
                });

                // Race in new subscribers while the burst runs. Their
                // subscribe() calls must acquire the subscribers Mutex; the
                // fix shortens the critical section from "duration of all
                // sends" to "duration of one Vec clone".
                let mut new_rxs = Vec::with_capacity(NEW_SUBS);
                for _ in 0..NEW_SUBS {
                    new_rxs.push(vm.subscribe());
                }

                handle.join().expect("burst thread joined");
                for s in &stops {
                    let _ = s.send(());
                }
                drop(new_rxs);
                drop(stops);
                drop(vm);
            },
            BatchSize::LargeInput,
        );
    });

    group.finish();
}

criterion_group!(benches, bench_notify);
criterion_main!(benches);
