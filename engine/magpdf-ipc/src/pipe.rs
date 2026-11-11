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

/// 引擎侧：创建服务端实例（字节流模式）。一个引擎只有这一个实例，只接启动它的主进程的一条连接；
/// `first_pipe_instance` 保证同名的管道不是别人先建好的。
pub fn create_server(name: &str) -> std::io::Result<Server> {
    ServerOptions::new()
        .pipe_mode(PipeMode::Byte)
        .first_pipe_instance(true)
        .create(name)
}

/// 引擎侧：等主进程连上；返回 `Ok(None)` 表示对方连上之后马上又断开了。
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
    async fn the_client_connects_and_a_second_server_with_the_same_name_is_refused() {
        let name = pipe_name(std::process::id(), &random_token(8).unwrap());
        let mut server = create_server(&name).unwrap();
        // 同名的管道已经有人建了：`first_pipe_instance` 让第二个创建失败，冒名顶替的服务端起不来。
        assert!(create_server(&name).is_err());

        let pending = tokio::spawn(async move {
            let accepted = accept(&mut server).await;
            assert_eq!(accepted.unwrap(), Some(()));
        });
        let mut errors = Vec::new();
        let mut connected = false;
        for _ in 0..200 {
            match connect_client(&name) {
                Ok(client) => {
                    drop(client);
                    connected = true;
                    break;
                }
                Err(e) => {
                    errors.push(e);
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            }
        }
        assert!(connected, "客户端连不上管道：{errors:?}");
        tokio::time::timeout(std::time::Duration::from_secs(5), pending)
            .await
            .expect("服务端没有接受连接")
            .unwrap();
    }
}
