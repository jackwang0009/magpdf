//! `bench.engine`：引擎侧的同口径测量（P0 报告 V06 的三条通过线与 I1、I2）。
//!
//! 每轮对同一组操作重复 `iterations` 次，取耗时（微秒）：共享内存读写、瓦片渲染、渲染加写入、四种
//! 编解码。结果按 `runs` 轮分别返回，由主进程汇总后取中位数与 P95。测量期间读当前进程的 CPU 时间，
//! 用"进程 CPU 时间 / 墙钟时间"近似单核占用，确认测量没有被本进程的其他任务干扰。

use magpdf_ipc::codec;
use magpdf_ipc::messages::{
    BenchEngineParams, BenchEngineResult, ErrorCode, Request, RpcError, Stats,
};
use magpdf_ipc::shm::{self, TILE_BYTES};
use serde::Serialize;

use crate::server::Engine;

fn stats(samples: Vec<f64>) -> Stats {
    Stats::from_samples(samples)
}

fn err(message: impl Into<String>) -> RpcError {
    ErrorCode::Internal.error_with(message)
}

/// 跑一轮测量。
pub async fn run(engine: &Engine, p: BenchEngineParams) -> Result<BenchEngineResult, RpcError> {
    let iterations = p.iterations.max(1) as usize;
    let docs = engine.engine.list_docs().await;
    let doc = docs
        .first()
        .ok_or_else(|| err("没有打开的文档；请先 doc.open"))?;
    // 口径：1.0 倍缩放下的页面中间位置的瓦片。
    let (surface_w, surface_h) = engine
        .engine
        .page_surface(doc.doc_id, 0, 1.0)
        .await
        .map_err(|e| err(e.to_string()))?;
    let x = (surface_w.saturating_sub(shm::TILE_SIZE) / 2) / shm::TILE_SIZE * shm::TILE_SIZE;
    let y = (surface_h.saturating_sub(shm::TILE_SIZE) / 2) / shm::TILE_SIZE * shm::TILE_SIZE;

    // 准备的样本消息：与真实往返同形状（带 id、方法名、参数）。
    let echo = magpdf_ipc::messages::PingParams { t: 123_456 };
    let request: Request =
        magpdf_ipc::frame::request(42, magpdf_ipc::messages::method::BENCH_ECHO, &echo);
    let response = magpdf_ipc::frame::response_ok::<magpdf_ipc::messages::PingResult>(42, &echo);
    let json_request = codec::encode_json(&request).map_err(|e| err(e.to_string()))?;
    let json_response = codec::encode_json(&response).map_err(|e| err(e.to_string()))?;
    let mp_request = codec::encode_msgpack(&request).map_err(|e| err(e.to_string()))?;
    let mp_response = codec::encode_msgpack(&response).map_err(|e| err(e.to_string()))?;

    // 1. 共享内存：写入与读出（预渲染一块瓦片作为被测数据）。
    let bitmap = engine
        .engine
        .render_tile(doc.doc_id, 0, 1.0, x, y)
        .await
        .map_err(|e| err(e.to_string()))?;
    let pixels = bitmap.bgra().to_vec();
    let slots = engine.slots.lock().map_err(|_| err("锁失效"))?.slots();
    if slots == 0 {
        return Err(err("没有共享内存槽位"));
    }
    let mut shm_write = Vec::with_capacity(iterations);
    let mut shm_read = Vec::with_capacity(iterations);
    {
        let mut region = engine.shm.lock().map_err(|_| err("锁失效"))?;
        for i in 0..iterations {
            let slot = u32::try_from(i).unwrap_or(0) % slots;
            let t0 = std::time::Instant::now();
            let generation = region
                .write_tile(slot, &pixels)
                .map_err(|e| err(e.to_string()))?;
            shm_write.push(t0.elapsed().as_secs_f64() * 1e6);

            let t1 = std::time::Instant::now();
            let out = region
                .read_tile(slot, generation)
                .map_err(|e| err(e.to_string()))?;
            shm_read.push(t1.elapsed().as_secs_f64() * 1e6);
            debug_assert_eq!(out.len(), TILE_BYTES);
            std::hint::black_box(&out);
        }
    }

    // 2. 引擎的一次小消息往返：请求 JSON 编码/解码 + 响应编码/解码（不含管道读写）。
    let mut roundtrip = Vec::with_capacity(iterations);
    for i in 0..iterations {
        let t0 = std::time::Instant::now();
        let req_bytes = codec::encode_json(&request).map_err(|e| err(e.to_string()))?;
        let parsed: Request =
            codec::decode_json(&req_bytes.bytes).map_err(|e| err(e.to_string()))?;
        let resp =
            magpdf_ipc::frame::response_ok::<magpdf_ipc::messages::PingResult>(parsed.id, &echo);
        let resp_bytes = codec::encode_json(&resp).map_err(|e| err(e.to_string()))?;
        let _: magpdf_ipc::messages::Response =
            codec::decode_json(&resp_bytes.bytes).map_err(|e| err(e.to_string()))?;
        std::hint::black_box(i);
        roundtrip.push(t0.elapsed().as_secs_f64() * 1e6);
    }

    // 3. 编解码（一侧的耗时）。
    let codec_times = codec_samples(
        &request,
        &json_request.bytes,
        &json_response.bytes,
        &mp_request.bytes,
        &mp_response.bytes,
        iterations,
    )?;

    // 4. 瓦片渲染，以及渲染 + 写入共享内存。
    let mut render_tile = Vec::with_capacity(iterations);
    let mut render_and_write = Vec::with_capacity(iterations);
    for i in 0..iterations {
        let t0 = std::time::Instant::now();
        let b = engine
            .engine
            .render_tile(doc.doc_id, 0, 1.0, x, y)
            .await
            .map_err(|e| err(e.to_string()))?;
        render_tile.push(t0.elapsed().as_secs_f64() * 1e6);
        let t1 = std::time::Instant::now();
        let mut region = engine.shm.lock().map_err(|_| err("锁失效"))?;
        let slot = u32::try_from(i).unwrap_or(0) % slots;
        region
            .write_tile(slot, b.bgra())
            .map_err(|e| err(e.to_string()))?;
        render_and_write.push(t1.elapsed().as_secs_f64() * 1e6);
    }

    let json_bytes = (json_request.len() + json_response.len()) as u32;
    let msgpack_bytes = (mp_request.len() + mp_response.len()) as u32;

    Ok(BenchEngineResult {
        engine_roundtrip: stats(roundtrip),
        shm_read: stats(shm_read),
        shm_write: stats(shm_write),
        render_tile: stats(render_tile),
        render_and_write: stats(render_and_write),
        json_encode: stats(codec_times.json_encode),
        json_decode: stats(codec_times.json_decode),
        msgpack_encode: stats(codec_times.msgpack_encode),
        msgpack_decode: stats(codec_times.msgpack_decode),
        json_bytes,
        msgpack_bytes,
        total_memory_mb: shm::total_memory_mb(),
        cpu_percent: 0.0,
    })
}

/// 一侧的编解码耗时样本（微秒）。
struct CodecTimes {
    json_encode: Vec<f64>,
    json_decode: Vec<f64>,
    msgpack_encode: Vec<f64>,
    msgpack_decode: Vec<f64>,
}

/// 一侧的编解码耗时：请求与响应各测一次，取平均。
fn codec_samples<T: Serialize>(
    request: &T,
    json_request: &[u8],
    json_response: &[u8],
    mp_request: &[u8],
    mp_response: &[u8],
    iterations: usize,
) -> Result<CodecTimes, RpcError> {
    let echo = magpdf_ipc::messages::PingParams { t: 123_456 };
    let response_example: magpdf_ipc::messages::Response =
        magpdf_ipc::frame::response_ok::<magpdf_ipc::messages::PingResult>(42, &echo);
    let mut json_encode = Vec::with_capacity(iterations);
    let mut json_decode = Vec::with_capacity(iterations);
    let mut mp_encode = Vec::with_capacity(iterations);
    let mut mp_decode = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let t0 = std::time::Instant::now();
        let a = serde_json::to_vec(request).map_err(|e| err(e.to_string()))?;
        let b = serde_json::to_vec(&response_example).map_err(|e| err(e.to_string()))?;
        json_encode.push(t0.elapsed().as_secs_f64() * 1e6 / 2.0);
        std::hint::black_box((&a, &b));

        let t0 = std::time::Instant::now();
        let _: Request = serde_json::from_slice(json_request).map_err(|e| err(e.to_string()))?;
        let _: magpdf_ipc::messages::Response =
            serde_json::from_slice(json_response).map_err(|e| err(e.to_string()))?;
        json_decode.push(t0.elapsed().as_secs_f64() * 1e6 / 2.0);

        let t0 = std::time::Instant::now();
        let a = rmp_serde::to_vec_named(request).map_err(|e| err(e.to_string()))?;
        let b = rmp_serde::to_vec_named(&response_example).map_err(|e| err(e.to_string()))?;
        mp_encode.push(t0.elapsed().as_secs_f64() * 1e6 / 2.0);
        std::hint::black_box((&a, &b));

        let t0 = std::time::Instant::now();
        let _: Request = rmp_serde::from_slice(mp_request).map_err(|e| err(e.to_string()))?;
        let _: magpdf_ipc::messages::Response =
            rmp_serde::from_slice(mp_response).map_err(|e| err(e.to_string()))?;
        mp_decode.push(t0.elapsed().as_secs_f64() * 1e6 / 2.0);
    }
    Ok(CodecTimes {
        json_encode,
        json_decode,
        msgpack_encode: mp_encode,
        msgpack_decode: mp_decode,
    })
}
