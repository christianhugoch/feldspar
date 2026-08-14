//! A local SMTP server that accepts one message, for tests that send mail.
//!
//! Principle 4 applied to the mail transport: the only way to know `lettre` was
//! wired up correctly — that the port is the one the settings named, that
//! credentials are attached when a username is configured, that the message
//! which arrives is the one that was built — is to put the bytes on a socket and
//! read them off the other end. A mock `Mailer` answers none of those questions.
//!
//! Deliberately a hand-written server rather than a mock-SMTP crate: what is
//! under test is what lettre writes to a socket, and anything that abstracts the
//! socket abstracts away the thing being checked.
//!
//! It lives here, beside [`TestDb`](crate::TestDb), because more than one crate
//! needs it — `sc-email` for the transport itself, `sc-server` for the Email
//! settings tab's test message — and a second copy would be a second definition
//! of what "enough SMTP" means.

use sc_error::{Error, Result};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

/// One SMTP conversation, as the server heard it.
#[derive(Debug, Default, Clone)]
pub struct SmtpMessage {
    /// Every command line before `DATA`, verbatim — the envelope (`MAIL FROM`,
    /// `RCPT TO`) and whether authentication happened.
    pub commands: Vec<String>,
    /// The `DATA` payload: headers and body, as they went over the wire.
    pub body: String,
}

impl SmtpMessage {
    /// Every command line joined, for a `contains` assertion that does not care
    /// which line an envelope command was on.
    pub fn transcript(&self) -> String {
        self.commands.join("\n")
    }

    /// Whether the client authenticated.
    pub fn authenticated(&self) -> bool {
        self.transcript().to_ascii_uppercase().contains("AUTH ")
    }
}

/// A listener that accepts exactly one message and then stops.
///
/// It answers `250` to everything it is not specifically interested in, which is
/// what makes it enough SMTP for a client to complete a send against, and it
/// stops as soon as the message is queued — deliberately *not* waiting for
/// `QUIT`, because a pooled client keeps the connection and does not quit until
/// its idle timeout.
pub struct TestSmtp {
    port: u16,
    /// `None` only after [`TestSmtp::message`] has taken it; see [`Drop`].
    server: Option<JoinHandle<SmtpMessage>>,
}

impl TestSmtp {
    /// Bind a listener on an ephemeral loopback port and start accepting.
    pub async fn start() -> Result<TestSmtp> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|e| Error::msg(format!("could not bind a test SMTP listener: {e}")))?;
        let port = listener
            .local_addr()
            .map_err(|e| Error::msg(format!("the test SMTP listener has no address: {e}")))?
            .port();
        let server = tokio::spawn(async move { converse(listener).await });
        Ok(TestSmtp {
            port,
            server: Some(server),
        })
    }

    /// The port to point `smtp_host`/`smtp_port` at. The host is `127.0.0.1`,
    /// and the security mode has to be `none`: this speaks no TLS, because what
    /// is under test is the SMTP conversation and not the handshake.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Wait for the message and return what arrived.
    pub async fn message(mut self) -> Result<SmtpMessage> {
        match self.server.take() {
            Some(server) => server
                .await
                .map_err(|e| Error::msg(format!("the test SMTP server did not finish: {e}"))),
            // Unreachable: `self` is consumed here and the only other taker is
            // `Drop`, which runs after.
            None => Err(Error::msg("the test SMTP server was already awaited")),
        }
    }
}

impl Drop for TestSmtp {
    /// A test that never waits for the message should not leave a task holding
    /// the port — which would make the *next* test's "nothing is listening
    /// there" case find something listening there.
    fn drop(&mut self) {
        if let Some(server) = self.server.take() {
            server.abort();
        }
    }
}

/// Speak enough SMTP to accept one message.
async fn converse(listener: TcpListener) -> SmtpMessage {
    let mut received = SmtpMessage::default();
    let Ok((stream, _)) = listener.accept().await else {
        return received;
    };
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    let mut in_data = false;

    if write
        .write_all(b"220 test.localhost ESMTP\r\n")
        .await
        .is_err()
    {
        return received;
    }
    while let Ok(Some(line)) = lines.next_line().await {
        if in_data {
            if line == "." {
                let _ = write.write_all(b"250 Ok: queued\r\n").await;
                break;
            }
            received.body.push_str(&line);
            received.body.push('\n');
            continue;
        }
        received.commands.push(line.clone());
        let upper = line.to_ascii_uppercase();
        let reply: &[u8] = if upper.starts_with("EHLO") {
            // The capability list is what makes a client offer AUTH at all — so
            // "did it authenticate" is a question this transcript can answer.
            b"250-test.localhost\r\n250-AUTH PLAIN LOGIN\r\n250 SMTPUTF8\r\n"
        } else if upper.starts_with("DATA") {
            in_data = true;
            b"354 End data with <CR><LF>.<CR><LF>\r\n"
        } else {
            b"250 Ok\r\n"
        };
        if write.write_all(reply).await.is_err() {
            break;
        }
    }
    received
}
