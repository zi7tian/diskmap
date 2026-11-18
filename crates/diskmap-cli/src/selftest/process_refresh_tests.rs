//! 只用注入快照验证刷新竞态和等待预算，不读取真实进程或修改环境。

use super::*;
use diskmap_core::i18n::Lang;
use std::collections::VecDeque;

fn processes(refreshing: bool, total: u64, files: u64) -> Value {
    json!({
        "total": total,
        "visible": total,
        "files": files,
        "refreshing": refreshing,
        "error": null,
    })
}

fn recorded(state: &Shared, refresh: ProcessRefresh, budget: Duration) -> Vec<Value> {
    record_process_checks(state, refresh, budget);
    let results = lock(state).results.clone();
    assert_eq!(results.len(), 2);
    results
}

#[test]
fn slow_refresh_waits_past_empty_and_partial_snapshots() {
    for lang in [Lang::Zh, Lang::En] {
        let _scope = i18n::scope(lang);
        let state = Arc::new(State::default());
        let budget = Duration::from_secs(1);
        let finished = processes(false, 23, 47);
        let mut snapshots =
            VecDeque::from([processes(true, 3, 5), finished.clone(), finished.clone()]);
        let mut calls = 0;
        let refresh = wait_for_processes(&state, processes(true, 0, 0), budget, |remaining| {
            assert!(!remaining.is_zero());
            assert!(remaining <= budget);
            calls += 1;
            Ok(snapshots.pop_front().expect("不应在已确认完成后继续轮询"))
        })
        .unwrap();
        assert!(!refresh.timed_out);
        assert_eq!(refresh.procs, finished);
        assert_eq!(calls, 3);
        assert!(
            recorded(&state, refresh, budget)
                .iter()
                .all(|r| r["ok"] == true)
        );
    }
}

#[test]
fn completed_flag_is_confirmed_with_a_fresh_nonempty_snapshot() {
    // App.status 的快照和 refreshing 来自两次读取；旧空快照配 false 不能直接判失败。
    for initially_refreshing in [true, false] {
        let state = Arc::new(State::default());
        let budget = Duration::from_secs(1);
        let finished = processes(false, 7, 11);
        let mut snapshots = VecDeque::from([processes(false, 0, 0), finished.clone()]);
        let mut calls = 0;
        let refresh = wait_for_processes(
            &state,
            processes(initially_refreshing, 0, 0),
            budget,
            |_| {
                calls += 1;
                Ok(snapshots.pop_front().expect("两份连续 false 应足够确认"))
            },
        )
        .unwrap();
        assert!(!refresh.timed_out);
        assert_eq!(refresh.procs, finished);
        assert_eq!(calls, 2, "初始 POST 快照不能充当 GET 确认");
        assert!(
            recorded(&state, refresh, budget)
                .iter()
                .all(|r| r["ok"] == true)
        );
    }
}

#[test]
fn refresh_resuming_between_confirmations_requires_two_new_completed_snapshots() {
    let state = Arc::new(State::default());
    let budget = Duration::from_secs(1);
    let finished = processes(false, 19, 37);
    let mut snapshots = VecDeque::from([
        processes(false, 0, 0),
        processes(true, 5, 8),
        finished.clone(),
        finished.clone(),
    ]);
    let mut calls = 0;
    let refresh = wait_for_processes(&state, processes(true, 0, 0), budget, |_| {
        calls += 1;
        Ok(snapshots.pop_front().expect("完成确认之后不应再发请求"))
    })
    .unwrap();
    assert!(!refresh.timed_out);
    assert_eq!(refresh.procs, finished);
    assert_eq!(calls, 4);
    assert!(
        recorded(&state, refresh, budget)
            .iter()
            .all(|r| r["ok"] == true)
    );
}

#[test]
fn refreshing_with_existing_counts_times_out_and_preserves_the_entire_last_state() {
    for lang in [Lang::Zh, Lang::En] {
        let _scope = i18n::scope(lang);
        let state = Arc::new(State::default());
        let budget = Duration::from_millis(20);
        let marker = format!("{}END-OF-LAST-STATE", "完整状态".repeat(100));
        let mut last = processes(true, 29, 53);
        last["diagnostic"] = json!(marker);
        let started = Instant::now();
        let refresh = wait_for_processes(&state, processes(true, 0, 0), budget, |remaining| {
            assert!(!remaining.is_zero());
            assert!(remaining <= budget);
            Ok(last.clone())
        })
        .unwrap();
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(refresh.timed_out);
        assert_eq!(refresh.procs, last);
        for result in recorded(&state, refresh, budget) {
            assert_eq!(result["ok"], false, "已有计数不能把刷新超时当成成功");
            let detail = result["detail"].as_str().unwrap();
            match lang {
                Lang::Zh => assert!(detail.contains("超时"), "{detail}"),
                Lang::En => assert!(detail.contains("Timed out"), "{detail}"),
            }
            assert!(detail.contains("refreshing"), "{detail}");
            assert!(detail.contains("true"), "{detail}");
            assert!(detail.contains("29"), "{detail}");
            assert!(detail.contains("53"), "{detail}");
            assert!(detail.contains(&marker), "末次状态不应被截断：{detail}");
            assert!(detail.chars().count() > 300);
        }
    }
}

#[test]
fn only_boolean_false_marks_refreshing_as_finished() {
    for value in [Value::Null, json!(0), json!("false")] {
        let state = Arc::new(State::default());
        let mut last = processes(false, 3, 5);
        last["refreshing"] = value;
        let budget = Duration::from_millis(10);
        let refresh =
            wait_for_processes(&state, last.clone(), budget, |_| Ok(last.clone())).unwrap();
        assert!(refresh.timed_out);
        assert!(
            recorded(&state, refresh, budget)
                .iter()
                .all(|r| r["ok"] == false)
        );
    }
}

#[test]
fn completed_but_empty_snapshot_still_fails_both_checks() {
    let state = Arc::new(State::default());
    let budget = Duration::from_secs(1);
    let empty = processes(false, 0, 0);
    let mut calls = 0;
    let refresh = wait_for_processes(&state, empty.clone(), budget, |_| {
        calls += 1;
        Ok(empty.clone())
    })
    .unwrap();
    assert!(!refresh.timed_out);
    assert_eq!(calls, 2);
    assert!(
        recorded(&state, refresh, budget)
            .iter()
            .all(|r| r["ok"] == false)
    );
}

#[test]
fn cancellation_before_waiting_never_fetches() {
    let state = Arc::new(State::default());
    state.canceled.store(true, Ordering::Release);
    let result = wait_for_processes(
        &state,
        processes(true, 0, 0),
        Duration::from_millis(10),
        |_| panic!("已取消时不能发出刷新请求"),
    );
    assert!(result.is_err());
    assert!(lock(&state).results.is_empty());
}

#[test]
fn cancellation_during_fetch_prevents_confirmation_requests() {
    let state = Arc::new(State::default());
    let mut calls = 0;
    let result = wait_for_processes(
        &state,
        processes(true, 0, 0),
        Duration::from_secs(1),
        |_| {
            calls += 1;
            state.canceled.store(true, Ordering::Release);
            Ok(processes(false, 3, 5))
        },
    );
    assert!(result.is_err());
    assert_eq!(calls, 1);
    assert!(lock(&state).results.is_empty());
}
