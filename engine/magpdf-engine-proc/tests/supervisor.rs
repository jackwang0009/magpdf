//! 主进程一侧的监督者（`magpdf_ipc::host::Supervisor`）对着真实的引擎进程跑：请求与事件、
//! 引擎被结束后自动重启并恢复文档、挂死时靠心跳发现（方案 9.3；《接口协议》第 2、4、7 节）。
// 测试代码允许 unwrap/expect（《工程规范》4）；集成测试不在 #[cfg(test)] 下，需显式放开。
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use magpdf_ipc::host::{EngineLaunch, HostEvent, Supervisor};
use magpdf_ipc::messages::{ErrorCode, event, method};
use magpdf_ipc::shm;
use serde_json::json;
use tokio::sync::mpsc::UnboundedReceiver;

/// 单次等待的上限。
const STEP_TIMEOUT: Duration = Duration::from_secs(30);

fn sample(name: &str) -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/corpus/synthetic")
        .join(name)
        .to_string_lossy()
        .into_owned()
}

/// 每个测试一份自己的恢复文件；心跳调得很密，好让挂死的用例几百毫秒内出结果。
fn launch(tag: &str) -> EngineLaunch {
    let recovery = std::env::temp_dir().join(format!(
        "magpdf-supervisor-{}-{tag}.json",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&recovery);
    let mut launch = EngineLaunch::new(
        PathBuf::from(env!("CARGO_BIN_EXE_magpdf-engine")),
        recovery,
        env!("CARGO_PKG_VERSION"),
    );
    launch.slots = 16;
    launch.heartbeat_interval = Duration::from_millis(100);
    launch.heartbeat_timeout = Duration::from_millis(400);
    launch
}

/// 等到一条满足条件的事件，中间不满足的丢掉。
async fn next_event(
    events: &mut UnboundedReceiver<HostEvent>,
    wanted: impl Fn(&HostEvent) -> bool,
) -> HostEvent {
    let wait = async {
        loop {
            let event = events.recv().await.expect("事件通道关了");
            if wanted(&event) {
                return event;
            }
        }
    };
    tokio::time::timeout(STEP_TIMEOUT, wait)
        .await
        .expect("等事件超时")
}

fn is_lifecycle(event: &HostEvent) -> bool {
    !matches!(event, HostEvent::Engine { .. })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn requests_events_and_tiles_go_through_the_supervisor() {
    let launch = launch("basic");
    let recovery = launch.recovery.clone();
    let (supervisor, mut events, restored) = Supervisor::start(launch).await.unwrap();
    assert!(restored.is_empty());
    assert!(supervisor.engine_pid().is_some());

    let doc = supervisor
        .request(
            method::DOC_OPEN,
            json!({ "path": sample("freight-bl-composite.pdf") }),
        )
        .await
        .unwrap();
    let doc_id = doc["docId"].as_u64().unwrap();

    // 多条请求同时在途，各自拿到自己的应答。
    let pings: Vec<_> = (0..20u64)
        .map(|t| {
            let supervisor = supervisor.clone();
            tokio::spawn(async move {
                supervisor
                    .request(method::SESSION_PING, json!({ "t": t }))
                    .await
            })
        })
        .collect();
    for (t, ping) in pings.into_iter().enumerate() {
        assert_eq!(ping.await.unwrap().unwrap()["t"], t);
    }

    // 渲染一块瓦片：应答之后来事件，像素从共享内存读。
    let result = supervisor
        .request(
            method::RENDER_TILES,
            json!({
                "docId": doc_id,
                "pageId": 0,
                "scale": 1.0,
                "tiles": [{ "x": 0, "y": 0 }],
                "priority": "visible",
            }),
        )
        .await
        .unwrap();
    let request_id = result["requestIds"][0].clone();
    let ready = next_event(&mut events, |e| {
        matches!(e, HostEvent::Engine { method, params }
            if method == event::RENDER_TILE_READY && params["requestId"] == request_id)
    })
    .await;
    let HostEvent::Engine { params, .. } = ready else {
        unreachable!()
    };
    let slot = u32::try_from(params["slot"].as_u64().unwrap()).unwrap();
    let generation = u32::try_from(params["generation"].as_u64().unwrap()).unwrap();
    assert_eq!(
        supervisor.read_tile(slot, generation).unwrap().len(),
        shm::TILE_BYTES
    );
    assert!(supervisor.read_tile(slot, generation + 2).is_err());

    // 引擎的错误原样带回来。
    let error = supervisor
        .request("no.such.method", json!({}))
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Protocol.code());

    // 正常退出之后不再重启。
    supervisor.shutdown().await;
    let deadline = Instant::now() + STEP_TIMEOUT;
    while supervisor.engine_pid().is_some() {
        assert!(Instant::now() < deadline, "shutdown 之后引擎还在");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let after = supervisor
        .request(method::SESSION_PING, json!({ "t": 1 }))
        .await
        .unwrap_err();
    assert_eq!(after.code, ErrorCode::EngineRestarted.code());
    let _ = std::fs::remove_file(recovery);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_killed_engine_is_replaced_and_documents_come_back() {
    let launch = launch("kill");
    let recovery = launch.recovery.clone();
    let (supervisor, mut events, _) = Supervisor::start(launch).await.unwrap();
    supervisor
        .request(
            method::DOC_OPEN,
            json!({ "path": sample("freight-bl-composite.pdf") }),
        )
        .await
        .unwrap();
    let old_pid = supervisor.engine_pid().unwrap();

    let killed = Command::new("taskkill")
        .args(["/PID", &old_pid.to_string(), "/F"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(killed.success());
    let started = Instant::now();

    assert_eq!(
        next_event(&mut events, is_lifecycle).await,
        HostEvent::Restarting
    );
    let HostEvent::Restored { restored } = next_event(&mut events, is_lifecycle).await else {
        panic!("应该收到 Restored");
    };
    // 用例里心跳是 100/400 ms；5 秒在开发机很宽裕，CI 共享机上拉起新进程偶尔更慢。
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "恢复用了 {:?}",
        started.elapsed()
    );
    assert_eq!(restored.len(), 1);
    assert_eq!(restored[0].path, sample("freight-bl-composite.pdf"));
    assert_ne!(supervisor.engine_pid(), Some(old_pid));

    // 新引擎里的文档直接可用。
    let snapshot = supervisor
        .request(method::DOC_SNAPSHOT, json!({ "docId": restored[0].doc_id }))
        .await
        .unwrap();
    assert_eq!(snapshot["pages"].as_array().unwrap().len(), 1);

    supervisor.shutdown().await;
    let _ = std::fs::remove_file(recovery);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hung_engine_is_detected_by_the_heartbeat() {
    let mut launch = launch("hang");
    launch.extra_args = vec!["--test-hooks".to_owned()];
    let recovery = launch.recovery.clone();
    let (supervisor, mut events, _) = Supervisor::start(launch).await.unwrap();
    let old_pid = supervisor.engine_pid().unwrap();

    // 把引擎的通信循环卡住 30 秒。进程还在、管道也没断，只有心跳能发现。
    let stalled = {
        let supervisor = supervisor.clone();
        tokio::spawn(async move {
            supervisor
                .request(method::BENCH_STALL, json!({ "ms": 30_000 }))
                .await
        })
    };
    let started = Instant::now();
    assert_eq!(
        next_event(&mut events, is_lifecycle).await,
        HostEvent::Restarting
    );
    assert!(matches!(
        next_event(&mut events, is_lifecycle).await,
        HostEvent::Restored { .. }
    ));
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "发现挂死并恢复用了 {:?}",
        started.elapsed()
    );
    // 卡住的那条请求以“引擎重启”结束，而不是一直等下去。
    let error = stalled.await.unwrap().unwrap_err();
    assert_eq!(error.code, ErrorCode::EngineRestarted.code());
    assert_ne!(supervisor.engine_pid(), Some(old_pid));

    // 没带 --test-hooks 的引擎不认这个方法：新引擎是用同样的参数拉起来的，所以这里仍然认。
    supervisor.shutdown().await;
    let _ = std::fs::remove_file(recovery);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_hooks_are_off_unless_asked_for() {
    let launch = launch("nohooks");
    let recovery = launch.recovery.clone();
    let (supervisor, _events, _) = Supervisor::start(launch).await.unwrap();
    let error = supervisor
        .request(method::BENCH_STALL, json!({ "ms": 1 }))
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Protocol.code());
    supervisor.shutdown().await;
    let _ = std::fs::remove_file(recovery);
}
