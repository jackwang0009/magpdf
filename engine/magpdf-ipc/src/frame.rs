//! 命名管道上的帧格式（《接口协议》3.2）：4 字节小端长度 + UTF-8 JSON。

use serde::Serialize;
use serde_json::Value;
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::messages::{
    JsonValue, MAX_FRAME_BYTES, Notification, Request, RequestId, Response, RpcError,
};

/// 帧编解码的错误。
#[derive(Debug, Error)]
pub enum FrameError {
    #[error("管道读写失败：{0}")]
    Io(#[from] std::io::Error),
    #[error("帧长 {len} 超过上限 {MAX_FRAME_BYTES}")]
    TooLarge { len: u64 },
    #[error("帧不是有效的 JSON-RPC 消息：{0}")]
    Json(#[from] serde_json::Error),
}

/// 把一条消息序列化成帧（长度前缀 + JSON 字节）。
pub fn encode<T: Serialize>(message: &T) -> Result<Vec<u8>, FrameError> {
    let body = serde_json::to_vec(message)?;
    Ok(encode_body(&body))
}

/// 给已经序列化好的 JSON 字节加上 4 字节长度前缀。
pub fn encode_body(body: &[u8]) -> Vec<u8> {
    let mut framed = Vec::with_capacity(body.len() + 4);
    // MAX_FRAME_BYTES 是 usize，序列化结果的长度必然能放进 u32（64 MB < 4 GB）。
    framed.extend_from_slice(&u32::try_from(body.len()).unwrap_or(u32::MAX).to_le_bytes());
    framed.extend_from_slice(body);
    framed
}

/// 写一帧。
pub async fn write_frame<W: AsyncWriteExt + Unpin, T: Serialize>(
    writer: &mut W,
    message: &T,
) -> Result<(), FrameError> {
    let framed = encode(message)?;
    writer.write_all(&framed).await?;
    writer.flush().await?;
    Ok(())
}

/// 读一帧的正文。连接关闭时返回 `Ok(None)`。
pub async fn read_body<R: AsyncReadExt + Unpin>(
    reader: &mut R,
) -> Result<Option<Vec<u8>>, FrameError> {
    let mut len_bytes = [0u8; 4];
    match reader.read_exact(&mut len_bytes).await {
        Ok(_) => {}
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::UnexpectedEof
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::BrokenPipe
            ) =>
        {
            return Ok(None);
        }
        Err(e) => return Err(FrameError::Io(e)),
    }
    let len = u64::from(u32::from_le_bytes(len_bytes));
    if len > MAX_FRAME_BYTES as u64 {
        return Err(FrameError::TooLarge { len });
    }
    // 长度已检查在 64 MB 以内。
    let mut body = vec![0u8; len as usize];
    reader.read_exact(&mut body).await?;
    Ok(Some(body))
}

/// 读一帧并解析成 JSON 值。
pub async fn read_value<R: AsyncReadExt + Unpin>(
    reader: &mut R,
) -> Result<Option<Value>, FrameError> {
    let Some(body) = read_body(reader).await? else {
        return Ok(None);
    };
    Ok(Some(serde_json::from_slice(&body)?))
}

/// 管道上收到的三类消息。
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    Request(Request),
    Response(Response),
    Notification(Notification),
}

impl Incoming {
    /// 从 JSON 值判定消息种类。带 `id` 的按 `method` 有无区分请求与响应，否则是通知。
    pub fn from_value(value: Value) -> Result<Self, FrameError> {
        let is_request = value.get("method").is_some();
        if is_request && value.get("id").is_some() {
            Ok(Self::Request(serde_json::from_value(value)?))
        } else if is_request {
            Ok(Self::Notification(serde_json::from_value(value)?))
        } else {
            Ok(Self::Response(serde_json::from_value(value)?))
        }
    }
}

/// 把参数或结果转成 [`JsonValue`]。序列化失败（不应发生）时得到 `None`。
pub fn json_value<T: Serialize>(value: &T) -> Option<JsonValue> {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| serde_json::from_value(v).ok())
}

/// 构造成功响应。
pub fn response_ok<T: Serialize>(id: RequestId, result: &T) -> Response {
    Response {
        jsonrpc: crate::messages::JsonRpcTag::V2,
        id,
        result: json_value(result),
        error: None,
    }
}

/// 构造错误响应。
pub fn response_err(id: RequestId, error: RpcError) -> Response {
    Response {
        jsonrpc: crate::messages::JsonRpcTag::V2,
        id,
        result: None,
        error: Some(error),
    }
}

/// 构造通知。
pub fn notification<T: Serialize>(method: &str, params: &T) -> Notification {
    Notification {
        jsonrpc: crate::messages::JsonRpcTag::V2,
        method: method.to_owned(),
        params: json_value(params),
    }
}

/// 构造请求。
pub fn request<T: Serialize>(id: RequestId, method: &str, params: &T) -> Request {
    Request {
        jsonrpc: crate::messages::JsonRpcTag::V2,
        id,
        method: method.to_owned(),
        params: json_value(params),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::JsonRpcTag;

    #[test]
    fn frame_roundtrip_through_a_buffer() {
        let req = request(7, "session.ping", &crate::messages::PingParams { t: 42 });
        let framed = encode(&req).unwrap();
        assert_eq!(
            u32::from_le_bytes(framed[..4].try_into().unwrap()),
            (framed.len() - 4) as u32
        );
        let value: Value = serde_json::from_slice(&framed[4..]).unwrap();
        assert_eq!(value["jsonrpc"], "2.0");
        assert_eq!(value["method"], "session.ping");
        assert_eq!(value["params"]["t"].as_f64(), Some(42.0));
        match Incoming::from_value(value).unwrap() {
            Incoming::Request(r) => assert_eq!(r, req),
            other => panic!("应解析为请求：{other:?}"),
        }
    }

    #[tokio::test]
    async fn async_read_write_preserves_messages() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        let note = notification(
            "tile.ready",
            &crate::messages::TileReady {
                request_id: 1,
                doc_id: 2,
                page_id: 0,
                x: 512,
                y: 0,
                slot: 3,
                generation: 4,
            },
        );
        write_frame(&mut a, &note).await.unwrap();
        write_frame(&mut a, &response_ok::<u32>(9, &5))
            .await
            .unwrap();
        drop(a);
        let first = read_value(&mut b).await.unwrap().unwrap();
        let second = read_value(&mut b).await.unwrap().unwrap();
        assert!(read_value(&mut b).await.unwrap().is_none());
        assert!(matches!(
            Incoming::from_value(first).unwrap(),
            Incoming::Notification(_)
        ));
        match Incoming::from_value(second).unwrap() {
            Incoming::Response(r) => {
                assert_eq!(r.id, 9);
                assert_eq!(
                    r.result,
                    Some(JsonValue::Number(serde_json::Number::from(5u64)))
                );
                assert_eq!(r.jsonrpc, JsonRpcTag::V2);
            }
            other => panic!("应解析为响应：{other:?}"),
        }
    }

    #[test]
    fn oversized_length_is_rejected() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let mut bytes = ((MAX_FRAME_BYTES + 1) as u32).to_le_bytes().to_vec();
        bytes.push(b'{');
        let err = rt.block_on(async {
            let mut cursor = std::io::Cursor::new(bytes);
            read_body(&mut cursor).await.unwrap_err()
        });
        assert!(matches!(err, FrameError::TooLarge { .. }));
    }

    #[test]
    fn stats_use_nearest_rank_p95() {
        let samples: Vec<f64> = (1..=100).map(f64::from).collect();
        let s = crate::messages::Stats::from_samples(samples);
        assert_eq!(s.median_us, 50.0);
        assert_eq!(s.p95_us, 95.0);
        assert_eq!(s.min_us, 1.0);
        assert_eq!(s.max_us, 100.0);
        assert_eq!(crate::messages::Stats::from_samples(Vec::new()).samples, 0);
    }
}
