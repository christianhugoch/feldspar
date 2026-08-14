//! Sending mail: a message, a transport seam, and SMTP (layer 6, design §18.2).
//!
//! Three things, and the split between them is the point:
//!
//! - [`Email`] is a message with no opinion about how it travels — addresses, a
//!   subject and one or both bodies. It is what an action produces.
//! - [`Mailer`] is the transport, one `async fn send`. Everything that sends
//!   mail goes through it, so changing provider stays a configuration change and
//!   the OAuth2 and Microsoft Graph transports §18.2 leaves for later are a new
//!   implementation rather than a rewrite.
//! - [`SmtpMailer`] is the one implementation that reaches a network, built from
//!   the [`EmailSettings`](sc_config::EmailSettings) an admin saved, and
//!   [`SettingsMailer`] is the one a server installs: it reads those settings
//!   afresh for every message, so changing them takes effect on the next one.
//!
//! Beside them is [`render_mjml`], which compiles an HTML body written as
//! [MJML](https://mjml.io) — the markup an email designer writes — into the
//! table markup a mail client will lay out. It is here rather than in the action
//! because "what an email body may be written in" is this crate's question, and
//! the system emails will want the same answer.
//!
//! The trait is not speculation about future providers: it is what lets an
//! action's tests assert *what would have been sent* without an SMTP server,
//! through [`RecordingMailer`]. That is why the recorder lives here beside the
//! real transport rather than in some test module — two crates use it.
//!
//! **Addresses are parsed, not concatenated.** [`parse_recipients`] splits a
//! rendered recipient string into mailboxes on the commas that separate them and
//! not on the ones inside a quoted display name, and names the address it
//! rejected. A template renders `to`, so the string this is handed is whatever a
//! row held, and "which address was wrong" is the only useful answer.

mod mjml;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use lettre::{
    AsyncSmtpTransport, AsyncTransport, Tokio1Executor,
    message::{Mailbox as LettreMailbox, MultiPart, SinglePart},
    transport::smtp::authentication::Credentials,
};
use sc_catalog::Catalog;
use sc_config::{EmailSettings, SmtpSecurity};
use sc_error::{Error, Result};

pub use mjml::render_mjml;
// The address vocabulary, re-exported: a crate that builds a message should not
// have to depend on the *settings* crate to name an address. Parsed there, and
// only there, because the from-address is a setting and a setting is checked
// where it is declared — this is that one grammar, borrowed.
pub use sc_config::{Mailbox, parse_mailbox};

/// One message, independent of how it is sent.
///
/// Both bodies are optional and at least one has to be present; that is checked
/// by [`Email::check`] rather than by the type, because the action that builds
/// one wants to report "a message needs a body" against the trigger the admin is
/// editing, not to be unable to construct the value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Email {
    /// Who the message is from.
    pub from: Mailbox,
    /// The primary recipients.
    pub to: Vec<Mailbox>,
    /// Carbon copies.
    pub cc: Vec<Mailbox>,
    /// Blind carbon copies.
    pub bcc: Vec<Mailbox>,
    /// The subject line, already rendered.
    pub subject: String,
    /// The `text/plain` body.
    pub text: Option<String>,
    /// The `text/html` body.
    pub html: Option<String>,
}

impl Email {
    /// A message with a from-address and nothing else, to be filled in.
    pub fn new(from: Mailbox) -> Email {
        Email {
            from,
            to: Vec::new(),
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: String::new(),
            text: None,
            html: None,
        }
    }

    /// What has to be true before a message can be sent at all.
    ///
    /// Separate from the transport so an action can run it while an admin is
    /// still looking at the form: a message with no recipients and a message
    /// with no body are both mistakes worth naming there rather than at send.
    pub fn check(&self) -> Result<()> {
        if self.to.is_empty() && self.cc.is_empty() && self.bcc.is_empty() {
            return Err(Error::invalid(
                "an email needs at least one recipient: `to`, `cc` or `bcc`",
            ));
        }
        if self.text.is_none() && self.html.is_none() {
            return Err(Error::invalid(
                "an email needs a body: set the HTML body, the text body, or both",
            ));
        }
        Ok(())
    }

    /// Every recipient, in header order — what a log line or a test assertion
    /// wants when it does not care which header an address was in.
    pub fn recipients(&self) -> Vec<&Mailbox> {
        self.to.iter().chain(&self.cc).chain(&self.bcc).collect()
    }
}

/// The transport: the one way a message leaves this system.
#[async_trait]
pub trait Mailer: Send + Sync {
    /// Send one message, or say why it could not be sent.
    async fn send(&self, email: &Email) -> Result<()>;

    /// The address this transport sends **as**, for a sender that does not name
    /// one of its own.
    ///
    /// On the trait rather than read from the settings by every caller, because
    /// the from-address is a property of the transport: it is the configured
    /// `email_from` for SMTP, and it would be the authorised mailbox for an
    /// OAuth2 or Graph transport, which is not a setting at all. It is also
    /// where "this installation sends no mail" is discovered — a `send_email`
    /// action asks for it before it builds a message, so an unconfigured
    /// installation is an error pointing at Settings → Email rather than a
    /// connection attempt to nowhere.
    ///
    /// Async and fallible because the answer is read at send time: an admin who
    /// changes the from-address gets it on the next message, not the next
    /// restart.
    async fn sender(&self) -> Result<Mailbox>;
}

/// Split a rendered recipient string into mailboxes.
///
/// Commas separate addresses — except inside a quoted display name, where
/// `"Lovelace, Ada" <ada@example.com>` is one recipient and splitting on every
/// comma would make it two nonsense ones. An empty string is no recipients
/// rather than an error: which headers a message needs is [`Email::check`]'s
/// question, and a `cc` template that rendered to nothing is not a mistake.
pub fn parse_recipients(raw: &str) -> Result<Vec<Mailbox>> {
    let mut out = Vec::new();
    for part in split_on_unquoted_commas(raw) {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        out.push(parse_mailbox(part).map_err(|e| {
            // The whole list is quoted as well as the offending part, because a
            // recipient string is *rendered* — the admin sees a template, and
            // knowing which of three addresses a row produced badly is the
            // difference between a fixable report and a shrug.
            Error::invalid(format!("recipient `{part}` in `{raw}` is not usable: {e}"))
        })?);
    }
    Ok(out)
}

/// Split on the commas that separate addresses, not the ones inside quotes.
fn split_on_unquoted_commas(raw: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut quoted = false;
    let mut escaped = false;
    for (index, ch) in raw.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' if quoted => escaped = true,
            '"' => quoted = !quoted,
            ',' if !quoted => {
                parts.push(&raw[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    parts.push(&raw[start..]);
    parts
}

/// SMTP, over [`lettre`].
///
/// Built from the stored settings, so what it sends through is what the admin
/// saved rather than what a caller passed in — which is what makes a test
/// message a test of the *configuration* (§18.2).
pub struct SmtpMailer {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    /// The configured `email_from`, kept because it is what
    /// [`sender`](Mailer::sender) answers.
    from: Mailbox,
}

impl SmtpMailer {
    /// Build a transport for these settings.
    ///
    /// The three security modes are lettre's three builders and nothing else:
    /// `tls` is implicit TLS from the first byte, `starttls` connects in the
    /// clear and upgrades, and `none` is `builder_dangerous` — named that way by
    /// lettre, and reachable here only because [`EmailSettings::check`] has
    /// already refused it in combination with credentials.
    pub fn new(settings: &EmailSettings) -> Result<SmtpMailer> {
        let smtp = |what: &str, e: lettre::transport::smtp::Error| {
            Error::config(format!(
                "could not {what} for SMTP server `{}`: {e}",
                settings.host
            ))
        };
        let mut builder = match settings.security {
            SmtpSecurity::Tls => AsyncSmtpTransport::<Tokio1Executor>::relay(&settings.host)
                .map_err(|e| smtp("open a TLS connection", e))?,
            SmtpSecurity::StartTls => {
                AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&settings.host)
                    .map_err(|e| smtp("open a STARTTLS connection", e))?
            }
            SmtpSecurity::None => {
                AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&settings.host)
            }
        }
        .port(settings.port);
        if settings.authenticates() {
            builder = builder.credentials(Credentials::new(
                settings.username.clone(),
                settings.password.clone(),
            ));
        }
        Ok(SmtpMailer {
            transport: builder.build(),
            from: settings.from.clone(),
        })
    }
}

#[async_trait]
impl Mailer for SmtpMailer {
    async fn sender(&self) -> Result<Mailbox> {
        Ok(self.from.clone())
    }

    async fn send(&self, email: &Email) -> Result<()> {
        let message = build_message(email)?;
        self.transport
            .send(message)
            .await
            // The transport's own words, verbatim: "connection refused",
            // "relay denied" and "authentication failed" are three different
            // problems with three different fixes, and no summary this crate
            // could write beats naming which one happened.
            .map_err(|e| Error::msg(format!("the mail server rejected the message: {e}")))?;
        Ok(())
    }
}

/// An [`Email`] as the MIME message that goes on the wire.
///
/// Public because it is the only place the message's *structure* is decided, and
/// a test that asserts the structure should assert this rather than a
/// reimplementation of it.
pub fn build_message(email: &Email) -> Result<lettre::Message> {
    email.check()?;
    let mut builder = lettre::Message::builder()
        .from(to_lettre(&email.from)?)
        .subject(email.subject.clone());
    for address in &email.to {
        builder = builder.to(to_lettre(address)?);
    }
    for address in &email.cc {
        builder = builder.cc(to_lettre(address)?);
    }
    for address in &email.bcc {
        builder = builder.bcc(to_lettre(address)?);
    }
    let body = match (&email.text, &email.html) {
        // Text first inside `multipart/alternative`: the parts are ordered
        // least-preferred first, and every mail client reads the last one it can
        // render. Reversed, a graphical client would show the plain text.
        (Some(text), Some(html)) => builder.multipart(MultiPart::alternative_plain_html(
            text.clone(),
            html.clone(),
        )),
        (Some(text), None) => builder.singlepart(SinglePart::plain(text.clone())),
        (None, Some(html)) => builder.singlepart(SinglePart::html(html.clone())),
        // Refused by `check` above; this arm exists because the type says it
        // might happen, not because it can.
        (None, None) => return Err(Error::invalid("an email needs a body")),
    };
    body.map_err(|e| Error::invalid(format!("could not build the message: {e}")))
}

/// Our mailbox as lettre's.
///
/// Parsed here rather than carried as lettre's type through the whole system,
/// for the reason §2 gives generally: nothing above this crate should have to
/// name a dependency's type to describe an address.
fn to_lettre(mailbox: &Mailbox) -> Result<LettreMailbox> {
    mailbox.to_header().parse::<LettreMailbox>().map_err(|e| {
        Error::invalid(format!(
            "`{}` is not a usable address: {e}",
            mailbox.to_header()
        ))
    })
}

/// The transport an installation actually has: whatever the **saved settings**
/// say, read afresh for every message.
///
/// This is the mailer a server installs, and the indirection is the point. A
/// transport built once at boot would be the settings as they were at boot: an
/// admin who fixes a password would have to restart the server for the fix to
/// take, and the Email section's own help text ("changes take effect on the next
/// message; nothing here needs a restart") would be false. Reading the settings
/// per message costs one config lookup against a catalog that already caches
/// them, and building an [`SmtpMailer`] opens no connection — the socket is the
/// `send` call's, not the constructor's.
///
/// It is also where "this installation sends no mail" is discovered, once, with
/// a message pointing at the screen that fixes it.
pub struct SettingsMailer {
    catalog: Arc<Catalog>,
}

impl SettingsMailer {
    /// A mailer over the settings stored in `catalog`.
    pub fn new(catalog: Arc<Catalog>) -> SettingsMailer {
        SettingsMailer { catalog }
    }

    /// The saved settings, or the one error that names what is missing and where
    /// to put it.
    ///
    /// `EmailSettings::load` already distinguishes the two failures this has to
    /// keep apart: `None` is "no `smtp_host`", which is an installation that was
    /// never configured, and an `Err` is settings that are configured *wrongly*,
    /// whose own message says which key is wrong. Collapsing them into one
    /// "email is not set up" would throw away the half that is actionable.
    async fn settings(&self) -> Result<EmailSettings> {
        EmailSettings::load(&self.catalog).await?.ok_or_else(|| {
            Error::invalid(
                "this installation has no mail transport: set an SMTP host in \
                 Settings → Email and save",
            )
        })
    }
}

#[async_trait]
impl Mailer for SettingsMailer {
    async fn sender(&self) -> Result<Mailbox> {
        Ok(self.settings().await?.from)
    }

    async fn send(&self, email: &Email) -> Result<()> {
        SmtpMailer::new(&self.settings().await?)?.send(email).await
    }
}

/// A [`Mailer`] that keeps what it was handed and sends nothing.
///
/// The way an action's behaviour is asserted without an SMTP server: the test
/// runs the trigger and then reads the message, which is a stronger statement
/// than "the call returned `Ok`".
#[derive(Debug)]
pub struct RecordingMailer {
    sent: Mutex<Vec<Email>>,
    /// The identity this stand-in claims — see
    /// [`sending_as`](RecordingMailer::sending_as).
    from: Mailbox,
}

impl Default for RecordingMailer {
    fn default() -> RecordingMailer {
        RecordingMailer {
            sent: Mutex::new(Vec::new()),
            // `.invalid` is the reserved TLD for exactly this (RFC 2606): a test
            // that does not care what the from-address is gets one that cannot
            // be a real address by construction, so a message that escaped into
            // a real transport would bounce rather than arrive from a domain
            // somebody owns.
            from: Mailbox {
                name: Some("Saltcorn".to_owned()),
                address: "recorder@example.invalid".to_owned(),
            },
        }
    }
}

impl RecordingMailer {
    /// A recorder with nothing in it.
    pub fn new() -> RecordingMailer {
        RecordingMailer::default()
    }

    /// A recorder that claims `from` as the transport's configured identity —
    /// for a test asserting that a message with no `from` of its own is sent as
    /// the installation's address.
    pub fn sending_as(from: Mailbox) -> RecordingMailer {
        RecordingMailer {
            from,
            ..RecordingMailer::default()
        }
    }

    /// Every message handed to this mailer, in order.
    pub fn sent(&self) -> Vec<Email> {
        match self.sent.lock() {
            Ok(sent) => sent.clone(),
            // A poisoned lock means a test panicked while holding it; the
            // messages recorded before that are still what the assertion wants.
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    /// The one message this mailer was handed, or an error saying how many it
    /// actually got — which is the assertion a test almost always means.
    pub fn only(&self) -> Result<Email> {
        let sent = self.sent();
        match sent.len() {
            1 => Ok(sent.into_iter().next().unwrap_or_else(|| {
                // Unreachable: the length was just checked.
                Email::new(Mailbox {
                    name: None,
                    address: String::new(),
                })
            })),
            n => Err(Error::invalid(format!(
                "expected exactly one message to have been sent, got {n}"
            ))),
        }
    }
}

#[async_trait]
impl Mailer for RecordingMailer {
    async fn sender(&self) -> Result<Mailbox> {
        Ok(self.from.clone())
    }

    async fn send(&self, email: &Email) -> Result<()> {
        // The same checks the real transport runs, so a test against the
        // recorder cannot pass on a message SMTP would refuse to build.
        build_message(email)?;
        match self.sent.lock() {
            Ok(mut sent) => sent.push(email.clone()),
            Err(poisoned) => poisoned.into_inner().push(email.clone()),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mailbox(raw: &str) -> Mailbox {
        parse_mailbox(raw).unwrap()
    }

    fn message(text: Option<&str>, html: Option<&str>) -> Email {
        Email {
            from: mailbox("Saltcorn <saltcorn@example.com>"),
            to: vec![mailbox("ada@example.com")],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "Receipt for order 7".to_owned(),
            text: text.map(str::to_owned),
            html: html.map(str::to_owned),
        }
    }

    #[test]
    fn a_recipient_list_is_split_on_the_commas_that_separate_addresses() {
        let parsed = parse_recipients("ada@example.com, Grace <grace@example.com>").unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].address, "ada@example.com");
        assert_eq!(parsed[1].name.as_deref(), Some("Grace"));

        // A comma inside a quoted display name is part of the name.
        let parsed = parse_recipients("\"Lovelace, Ada\" <ada@example.com>").unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].name.as_deref(), Some("Lovelace, Ada"));

        // An empty `cc` is no recipients, not an error.
        assert!(parse_recipients("").unwrap().is_empty());
        assert!(parse_recipients("  , ").unwrap().is_empty());
    }

    #[test]
    fn a_rejected_address_is_named_in_the_error() {
        let err = parse_recipients("ada@example.com, not-an-address, grace@example.com")
            .unwrap_err()
            .to_string();
        assert!(err.contains("not-an-address"), "{err}");
        // …and the whole list, so the admin can see which template produced it.
        assert!(err.contains("grace@example.com"), "{err}");
    }

    #[test]
    fn a_message_needs_a_recipient_and_a_body() {
        let mut email = message(Some("hello"), None);
        email.to.clear();
        assert!(email.check().unwrap_err().to_string().contains("recipient"));

        let email = message(None, None);
        assert!(email.check().unwrap_err().to_string().contains("body"));

        // A `bcc`-only message is a message: that is what a blind copy is.
        let mut email = message(Some("hello"), None);
        email.to.clear();
        email.bcc.push(mailbox("ada@example.com"));
        email.check().unwrap();
    }

    /// The headers and the MIME structure, for each of the three body shapes.
    #[test]
    fn the_built_message_has_the_headers_and_the_structure() {
        let both = String::from_utf8(
            build_message(&Email {
                cc: vec![mailbox("Grace <grace@example.com>")],
                bcc: vec![mailbox("audit@example.com")],
                ..message(Some("plain words"), Some("<p>rich words</p>"))
            })
            .unwrap()
            .formatted(),
        )
        .unwrap();
        assert!(
            both.contains("From: Saltcorn <saltcorn@example.com>"),
            "{both}"
        );
        assert!(both.contains("To: ada@example.com"), "{both}");
        assert!(both.contains("Cc: Grace <grace@example.com>"), "{both}");
        // Bcc is a recipient, not a header: it must not reach the message.
        assert!(!both.contains("audit@example.com"), "{both}");
        assert!(both.contains("Subject: Receipt for order 7"), "{both}");
        assert!(both.contains("multipart/alternative"), "{both}");
        // Text before HTML: `alternative` parts are least-preferred first, and a
        // graphical client renders the last one it understands.
        let text_at = both.find("text/plain").unwrap();
        let html_at = both.find("text/html").unwrap();
        assert!(text_at < html_at, "text should come first:\n{both}");

        let text_only = String::from_utf8(
            build_message(&message(Some("plain"), None))
                .unwrap()
                .formatted(),
        )
        .unwrap();
        assert!(text_only.contains("text/plain"), "{text_only}");
        assert!(!text_only.contains("multipart"), "{text_only}");

        let html_only = String::from_utf8(
            build_message(&message(None, Some("<p>hi</p>")))
                .unwrap()
                .formatted(),
        )
        .unwrap();
        assert!(html_only.contains("text/html"), "{html_only}");
        assert!(!html_only.contains("multipart"), "{html_only}");
    }

    #[tokio::test]
    async fn the_recorder_keeps_what_it_was_handed() {
        let mailer = RecordingMailer::new();
        assert!(mailer.only().is_err());
        mailer.send(&message(Some("one"), None)).await.unwrap();
        assert_eq!(mailer.only().unwrap().text.as_deref(), Some("one"));
        mailer.send(&message(Some("two"), None)).await.unwrap();
        assert_eq!(mailer.sent().len(), 2);
        assert!(mailer.only().is_err());
    }

    /// The recorder runs the same checks the wire does, so a test that passes
    /// against it is a test of a message SMTP would have accepted.
    #[tokio::test]
    async fn the_recorder_refuses_what_smtp_would_refuse() {
        let mailer = RecordingMailer::new();
        assert!(mailer.send(&message(None, None)).await.is_err());
        assert!(mailer.sent().is_empty());
    }
}
