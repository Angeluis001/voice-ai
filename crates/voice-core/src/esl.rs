//! FreeSWITCH event socket (ESL) client: authenticate, run one command, disconnect.
use crate::config::EslSettings;
use std::time::Duration;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::TcpStream,
};

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum EslError {
    #[error("esl_not_configured")]
    NotConfigured,
    #[error("esl_timeout")]
    Timeout,
    #[error("esl_auth_failed: {0}")]
    AuthFailed(String),
    #[error("esl_closed")]
    Closed,
    #[error("esl_io: {0}")]
    Io(String),
}

struct Frame {
    content_type: String,
    reply_text: String,
    body: String,
}

async fn read_frame(reader: &mut BufReader<TcpStream>) -> Result<Frame, EslError> {
    let mut frame = Frame {
        content_type: String::new(),
        reply_text: String::new(),
        body: String::new(),
    };
    let mut content_length = 0usize;
    let mut saw_header = false;
    loop {
        let mut line = String::new();
        let read = reader
            .read_line(&mut line)
            .await
            .map_err(|e| EslError::Io(e.to_string()))?;
        if read == 0 {
            return Err(EslError::Closed);
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            if saw_header {
                break;
            }
            continue;
        }
        saw_header = true;
        if let Some((name, value)) = line.split_once(':') {
            let value = value.trim();
            match name.trim().to_ascii_lowercase().as_str() {
                "content-type" => frame.content_type = value.to_string(),
                "reply-text" => frame.reply_text = value.to_string(),
                "content-length" => content_length = value.parse().unwrap_or(0),
                _ => {}
            }
        }
    }
    if content_length > 0 {
        let mut body = vec![0u8; content_length];
        reader
            .read_exact(&mut body)
            .await
            .map_err(|e| EslError::Io(e.to_string()))?;
        frame.body = String::from_utf8_lossy(&body).into_owned();
    }
    Ok(frame)
}

#[derive(Debug, Clone)]
pub struct EslClient {
    settings: EslSettings,
    timeout: Duration,
}

impl EslClient {
    pub fn new(settings: EslSettings) -> Self {
        Self {
            settings,
            timeout: Duration::from_millis(1500),
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Run `api <command>` and return the response body.
    pub async fn api(&self, command: &str) -> Result<String, EslError> {
        self.run("api", command).await
    }

    /// Run `bgapi <command>` and return the reply line (`+OK Job-UUID: ...`).
    pub async fn bgapi(&self, command: &str) -> Result<String, EslError> {
        self.run("bgapi", command).await
    }

    async fn run(&self, verb: &str, command: &str) -> Result<String, EslError> {
        if self.settings.password.is_empty() || command.is_empty() {
            return Err(EslError::NotConfigured);
        }
        tokio::time::timeout(self.timeout, self.exchange(verb, command))
            .await
            .map_err(|_| EslError::Timeout)?
    }

    async fn exchange(&self, verb: &str, command: &str) -> Result<String, EslError> {
        let stream = TcpStream::connect((self.settings.host.as_str(), self.settings.port))
            .await
            .map_err(|e| EslError::Io(e.to_string()))?;
        let mut reader = BufReader::new(stream);
        async fn write(reader: &mut BufReader<TcpStream>, text: String) -> Result<(), EslError> {
            reader
                .get_mut()
                .write_all(text.as_bytes())
                .await
                .map_err(|e| EslError::Io(e.to_string()))
        }

        loop {
            let frame = read_frame(&mut reader).await?;
            if frame.content_type == "auth/request" {
                break;
            }
        }
        write(&mut reader, format!("auth {}\n\n", self.settings.password)).await?;
        let auth = read_frame(&mut reader).await?;
        if !auth.reply_text.starts_with("+OK") {
            return Err(EslError::AuthFailed(auth.reply_text));
        }

        write(&mut reader, format!("{verb} {command}\n\n")).await?;
        loop {
            let frame = read_frame(&mut reader).await?;
            match frame.content_type.as_str() {
                "api/response" => return Ok(frame.body.trim().to_string()),
                "command/reply" => {
                    return Ok(format!("{} {}", frame.reply_text, frame.body)
                        .trim()
                        .to_string())
                }
                _ => {}
            }
        }
    }
}

/// Clear queued AI audio on a call without closing the audio fork.
/// (`uuid_break` must never be used here: it can end the silence stream and hang up.)
pub fn stop_play_command(channel_uuid: &str) -> String {
    format!("uuid_audio_fork {channel_uuid} stop_play")
}

/// Hand the live call to a human extension.
pub fn transfer_command(channel_uuid: &str, extension: &str) -> String {
    format!("uuid_transfer {channel_uuid} {extension} XML mrf")
}

/// A scripted FreeSWITCH event socket for tests: accepts connections, checks the
/// password, records each command and answers `+OK`.
pub mod fake {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tokio::net::TcpListener;

    pub struct FakeEsl {
        pub port: u16,
        pub commands: Arc<Mutex<Vec<String>>>,
    }

    pub async fn start(password: &'static str) -> FakeEsl {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fake esl");
        let port = listener.local_addr().unwrap().port();
        let commands = Arc::new(Mutex::new(Vec::new()));
        let log = commands.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let log = log.clone();
                tokio::spawn(async move {
                    let mut reader = BufReader::new(stream);
                    let _ = reader
                        .get_mut()
                        .write_all(b"Content-Type: auth/request\n\n")
                        .await;
                    let mut line = String::new();
                    loop {
                        line.clear();
                        if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                            return;
                        }
                        let text = line.trim().to_string();
                        if text.is_empty() {
                            continue;
                        }
                        let reply = if let Some(given) = text.strip_prefix("auth ") {
                            if given == password {
                                "Content-Type: command/reply\nReply-Text: +OK accepted\n\n"
                                    .to_string()
                            } else {
                                "Content-Type: command/reply\nReply-Text: -ERR invalid\n\n"
                                    .to_string()
                            }
                        } else if let Some(cmd) = text.strip_prefix("api ") {
                            log.lock().unwrap().push(cmd.to_string());
                            "Content-Type: api/response\nContent-Length: 3\n\n+OK".to_string()
                        } else if let Some(cmd) = text.strip_prefix("bgapi ") {
                            log.lock().unwrap().push(cmd.to_string());
                            "Content-Type: command/reply\nReply-Text: +OK Job-UUID: job-1\n\n"
                                .to_string()
                        } else {
                            continue;
                        };
                        if reader.get_mut().write_all(reply.as_bytes()).await.is_err() {
                            return;
                        }
                    }
                });
            }
        });
        FakeEsl { port, commands }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client(port: u16, password: &str) -> EslClient {
        EslClient::new(EslSettings {
            host: "127.0.0.1".into(),
            port,
            password: password.into(),
        })
    }

    #[tokio::test]
    async fn api_command_round_trip() {
        let esl = fake::start("pw").await;
        let body = client(esl.port, "pw")
            .api(&stop_play_command("abc"))
            .await
            .unwrap();
        assert_eq!(body, "+OK");
        assert_eq!(
            *esl.commands.lock().unwrap(),
            vec!["uuid_audio_fork abc stop_play"]
        );
    }

    #[tokio::test]
    async fn bgapi_returns_job_reply() {
        let esl = fake::start("pw").await;
        let reply = client(esl.port, "pw").bgapi("originate x y").await.unwrap();
        assert_eq!(reply, "+OK Job-UUID: job-1");
    }

    #[tokio::test]
    async fn wrong_password_is_an_error() {
        let esl = fake::start("pw").await;
        let err = client(esl.port, "nope").api("status").await.unwrap_err();
        assert!(matches!(err, EslError::AuthFailed(_)), "{err:?}");
        assert!(esl.commands.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn empty_password_is_not_configured() {
        assert_eq!(
            client(1, "").api("status").await.unwrap_err(),
            EslError::NotConfigured
        );
    }

    #[tokio::test]
    async fn unreachable_host_fails_fast() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let err = client(port, "pw").api("status").await.unwrap_err();
        assert!(
            matches!(err, EslError::Io(_) | EslError::Timeout),
            "{err:?}"
        );
    }

    #[test]
    fn command_strings() {
        assert_eq!(
            transfer_command("u1", "1000"),
            "uuid_transfer u1 1000 XML mrf"
        );
    }
}
