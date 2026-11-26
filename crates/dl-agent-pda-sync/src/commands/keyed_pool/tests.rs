use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

#[test]
fn one_key_runs_in_order_and_keys_run_side_by_side() {
    let log = Mutex::new(Vec::new());
    let running = AtomicUsize::new(0);
    let peak = AtomicUsize::new(0);
    run(
        4,
        |(key, n): (String, usize)| {
            let now = running.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(now, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(5));
            log.lock().unwrap().push((key, n));
            running.fetch_sub(1, Ordering::SeqCst);
        },
        |q| {
            for n in 0..10 {
                for key in ["a", "b", "c", "d", "e"] {
                    q.submit(key, (key.to_owned(), n));
                }
            }
        },
    );
    let log = log.into_inner().unwrap();
    assert_eq!(log.len(), 50, "every queued job ran before run returned");
    for key in ["a", "b", "c", "d", "e"] {
        let order: Vec<usize> = log
            .iter()
            .filter(|(k, _)| k == key)
            .map(|(_, n)| *n)
            .collect();
        assert_eq!(
            order,
            (0..10).collect::<Vec<_>>(),
            "key {key} kept its order"
        );
    }
    let peak = peak.load(Ordering::SeqCst);
    assert!(peak > 1 && peak <= 4, "bounded parallelism, got {peak}");
}

#[test]
fn one_key_never_runs_twice_at_once() {
    let running = AtomicUsize::new(0);
    run(
        8,
        |()| {
            assert_eq!(running.fetch_add(1, Ordering::SeqCst), 0);
            std::thread::sleep(Duration::from_millis(1));
            running.fetch_sub(1, Ordering::SeqCst);
        },
        |q| {
            for _ in 0..20 {
                q.submit("check:1:7", ());
            }
        },
    );
}

#[test]
fn a_job_that_runs_alone_waits_for_earlier_jobs_and_holds_back_later_ones() {
    // (label, alone?) in submit order; the log records start/end of each job.
    let log = Mutex::new(Vec::<String>::new());
    let running = AtomicUsize::new(0);
    run(
        4,
        |(label, alone): (&'static str, bool)| {
            let now = running.fetch_add(1, Ordering::SeqCst) + 1;
            if alone {
                assert_eq!(now, 1, "{label} ran beside another job");
            }
            log.lock().unwrap().push(format!("start {label}"));
            std::thread::sleep(Duration::from_millis(10));
            log.lock().unwrap().push(format!("end {label}"));
            running.fetch_sub(1, Ordering::SeqCst);
        },
        |q| {
            q.submit("check:1:7", ("a1", false));
            q.submit("check:1:8", ("b1", false));
            q.submit_alone(("merge", true));
            q.submit("check:1:8", ("b2", false));
            q.submit("check:1:9", ("c1", false));
            q.submit_alone(("rvc", true));
            q.submit("check:1:7", ("a2", false));
        },
    );
    let log = log.into_inner().unwrap();
    let at = |e: &str| log.iter().position(|l| l == e).unwrap();
    assert!(at("start merge") > at("end a1") && at("start merge") > at("end b1"));
    assert!(at("start b2") > at("end merge") && at("start c1") > at("end merge"));
    assert!(at("start rvc") > at("end b2") && at("start rvc") > at("end c1"));
    assert!(at("start a2") > at("end rvc"));
    assert_eq!(log.len(), 14, "every job ran: {log:?}");
}

#[test]
fn a_panicking_job_frees_its_key_and_the_panic_surfaces_after_the_drain() {
    let ran = Mutex::new(Vec::new());
    let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
        run(
            2,
            |n: usize| {
                assert!(n != 1, "job 1 fails");
                ran.lock().unwrap().push(n);
            },
            |q| {
                for n in 0..4 {
                    q.submit("check:1:7", n);
                }
                q.submit_alone(4);
                q.submit("check:1:7", 5);
            },
        );
    }));
    assert!(outcome.is_err(), "the job's panic is raised again");
    assert_eq!(
        ran.into_inner().unwrap(),
        [0, 2, 3, 4, 5],
        "the key's later jobs still ran, in order"
    );
}

#[test]
fn in_flight_counts_held_jobs_against_the_intake_limit() {
    assert_eq!(intake_limit(8), 32);
    assert_eq!(intake_limit(0), INTAKE_PER_WORKER);
    let gate = Mutex::new(());
    let held = gate.lock().unwrap();
    std::thread::scope(|s| {
        s.spawn(|| {
            run(
                1,
                |()| drop(gate.lock().unwrap()),
                |q| {
                    q.submit("a", ());
                    q.submit_alone(());
                    q.submit("b", ());
                    assert_eq!(q.in_flight(), 3);
                },
            );
        });
        std::thread::sleep(Duration::from_millis(20));
        drop(held);
    });
}
