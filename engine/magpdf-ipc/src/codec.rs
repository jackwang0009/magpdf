//! JSON 与 MessagePack 的对照编解码（《接口协议》待定项 I1）。
//!
//! 协议正式采用 JSON；MessagePack（`rmp-serde`，MIT）只用在这里做同口径的对照测量，
//! 不改变协议，也不进入主进程到引擎的实际通信路径。

use serde::Serialize;
use serde::de::DeserializeOwned;

/// 一侧的编码结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Encoded {
    pub bytes: Vec<u8>,
}

impl Encoded {
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
}

/// 把消息编码成 UTF-8 JSON。
pub fn encode_json<T: Serialize>(value: &T) -> Result<Encoded, serde_json::Error> {
    Ok(Encoded {
        bytes: serde_json::to_vec(value)?,
    })
}

/// 从 UTF-8 JSON 解码。
pub fn decode_json<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, serde_json::Error> {
    serde_json::from_slice(bytes)
}

/// 把消息编码成 MessagePack（对照用）。引擎的基准里另有直接调用 `rmp_serde` 的同口径测量，这里保留给\n/// 两边对照协议的单元测试。
pub fn encode_msgpack<T: Serialize>(value: &T) -> Result<Encoded, rmp_serde::encode::Error> {
    Ok(Encoded {
        bytes: rmp_serde::to_vec_named(value)?,
    })
}

/// 从 MessagePack 解码（对照用）。
pub fn decode_msgpack<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, rmp_serde::decode::Error> {
    rmp_serde::from_slice(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::{Request, RequestId};

    #[test]
    fn json_and_msgpack_carry_the_same_request() {
        let req = crate::frame::request(
            RequestId::from(17u8),
            crate::messages::method::BENCH_ECHO,
            &crate::messages::EchoParams { t: 1234 },
        );
        let json = encode_json(&req).unwrap();
        let packed = encode_msgpack(&req).unwrap();
        assert!(packed.len() < json.len());
        let from_json: Request = decode_json(&json.bytes).unwrap();
        let from_packed: Request = decode_msgpack(&packed.bytes).unwrap();
        assert_eq!(from_json, req);
        assert_eq!(from_packed, req);
    }
}
