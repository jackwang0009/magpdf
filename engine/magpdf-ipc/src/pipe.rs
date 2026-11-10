//! 命名管道的服务端与客户端（《接口协议》3.2）。
//!
//! 管道名 `\\.\pipe\magpdf-<主进程 PID>-<随机串>`，主进程启动引擎时通过命令行传给引擎。P0 原型用系统默认
//! 的安全描述符（只允许当前用户的令牌连接，但没有显式限制）；与规范要求的"显式只允许当前用户访问"的
//! 差距见 P0 报告 V06。

use tokio::net::windows::named_pipe::{
    ClientOptions, NamedPipeClient, NamedPipeServer, PipeMode, ServerOptions,
};

/// 主进程侧连上引擎后的连接。
pub type Client = NamedPipeClient;

/// 引擎侧接受后的连接。
pub type Server = NamedPipeServer;

const ALPHABET: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";

/// 用系统随机源生成 `len` 位小写字母数字串（管道名与共享内存名里的随机部分）。
pub fn random_token(len: usize) -> std::io::Result<String> {
    let mut bytes = vec![0u8; len];
    crate::shm::win32::system_random(&mut bytes)?;
    Ok(bytes
        .iter()
        .map(|b| char::from(ALPHABET[usize::from(*b) % ALPHABET.len()]))
        .collect())
}

/// 构造管道名。
pub fn pipe_name(host_pid: u32, token: &str) -> String {
    format!(r"\\.\pipe\magpdf-{host_pid}-{token}")
}

/// 引擎侧：创建第一个服务端实例（字节流模式）。
pub fn create_server(name: &str) -> std::io::Result<Server> {
    ServerOptions::new()
        .pipe_mode(PipeMode::Byte)
        .first_pipe_instance(true)
        .create(name)
}

/// 引擎侧：再接一个服务端实例（上一个客户端断开后调用）。
pub fn recreate(name: &str) -> std::io::Result<Server> {
    ServerOptions::new().pipe_mode(PipeMode::Byte).create(name)
}

/// 引擎侧：等一个客户端连上；返回 `Ok(None)` 表示主进程已经退出（无法再创建管道实例）。
pub async fn accept(server: &mut Server) -> std::io::Result<Option<()>> {
    match server.connect().await {
        Ok(()) => Ok(Some(())),
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(None),
        Err(e) => Err(e),
    }
}

/// 主进程侧：连接引擎的管道。引擎还没起来时返回 `ERROR_FILE_NOT_FOUND`（os error 2）。
pub fn connect_client(name: &str) -> std::io::Result<Client> {
    ClientOptions::new().pipe_mode(PipeMode::Byte).open(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipe_names_follow_the_protocol() {
        assert_eq!(pipe_name(4242, "abc123"), r"\\.\pipe\magpdf-4242-abc123");
        let token = random_token(16).unwrap();
        assert_eq!(token.len(), 16);
        assert!(
            token
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        );
        assert_ne!(token, random_token(16).unwrap());
    }

    #[tokio::test]
    async fn client_can_connect_twice() {
        let name = pipe_name(std::process::id(), &random_token(8).unwrap());
        let mut server = create_server(&name).unwrap();
        let pending = tokio::spawn({
            let name = name.clone();
            async move {
                let accepted = accept(&mut server).await;
                assert_eq!(accepted.unwrap(), Some(()));
                drop(server);
                let mut next = recreate(&name).unwrap();
                let accepted = accept(&mut next).await;
                assert_eq!(accepted.unwrap(), Some(()));
            }
        });
        let mut errors = Vec::new();
        let mut connected = 0;
        for _ in 0..200 {
            if connected == 2 {
                break;
            }
            match connect_client(&name) {
                Ok(client) => {
                    drop(client);
                    connected += 1;
                }
                Err(e) => {
                    errors.push(e);
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            }
        }
        assert_eq!(connected, 2, "客户端连不上管道：{errors:?}");
        tokio::time::timeout(std::time::Duration::from_secs(5), pending)
            .await
            .expect("服务端没有接受两个连接")
            .unwrap();
    }
}
