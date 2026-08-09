//! Where this deployment's applications are reachable in a browser.
//!
//! An application is served at `<subdomain>.<base-domain>` (design §13.2), and
//! that URL is a fact about the *deployment*, not about the app: the base
//! domain, the port the server bound, and whether it is behind TLS. Nothing in
//! the data layer needs it — and yet it is here, for the same reason the schema
//! observer and the table-event sink are: it is a process-wide fact that two
//! layers which cannot name each other both need, and the [`Catalog`] is the
//! handle they both already hold.
//!
//! The two layers here are the **server**, which is told the base domain on its
//! command line, and the **project generator** in `sc-app`, which writes an
//! application's `AGENTS.md` and `src/saltcorn/README.md` and has to tell a
//! developer — or their coding agent — which URL to open. Threading an origin
//! through `scaffold_app`, `emit_react_runtime`, `emit_app_client`,
//! `build_application` and every one of their callers would put a parameter that
//! is about *documentation* into the signature of everything that builds; every
//! one of those functions already takes a `&Catalog`.
//!
//! It is set once, at boot, by whichever process is doing the work: `saltcorn
//! serve` from its own configuration, and the command-line build from the
//! `saltcorn.toml` environment it was pointed at. That last part is the point of
//! storing it rather than passing it from the server alone — a `saltcorn
//! build-app` that rewrote the generated documentation *without* the URL would
//! be worse than one that never wrote it, because the two builds would disagree.
//!
//! Unset is a normal state, not a misconfiguration: a server with no
//! `--base-domain` serves no applications at all, and the documentation then
//! says what to substitute instead of inventing a hostname.
//!
//! [`Catalog`]: crate::Catalog

/// The origin an application is served on: `https://<subdomain>.<base_domain>`,
/// with the port when it is not the scheme's default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicOrigin {
    /// The server's base domain — `example.com`, so the app `blog` is at
    /// `blog.example.com` (§13.2).
    pub base_domain: String,
    /// The port the server is reachable on.
    pub port: u16,
    /// Whether it is served over TLS.
    pub secure: bool,
}

impl PublicOrigin {
    /// An origin on `base_domain`, port `port`, over plain HTTP.
    pub fn new(base_domain: impl Into<String>, port: u16) -> PublicOrigin {
        PublicOrigin {
            base_domain: base_domain.into(),
            port,
            secure: false,
        }
    }

    /// The same origin over TLS, returning `self` for chaining.
    pub fn secure(mut self, secure: bool) -> PublicOrigin {
        self.secure = secure;
        self
    }

    /// `http` or `https`.
    pub fn scheme(&self) -> &'static str {
        if self.secure { "https" } else { "http" }
    }

    /// The host an application is served on: `blog.example.com`.
    pub fn host_for(&self, subdomain: &str) -> String {
        format!("{subdomain}.{}", self.base_domain)
    }

    /// The URL to open an application at: `http://blog.example.com:3000`.
    ///
    /// The port is **omitted when it is the scheme's default**, because a URL
    /// with `:443` in it is one a reader has to think about, and this one is
    /// written into documentation to be pasted rather than read.
    pub fn url_for(&self, subdomain: &str) -> String {
        let default_port = if self.secure { 443 } else { 80 };
        let host = self.host_for(subdomain);
        if self.port == default_port {
            format!("{}://{host}", self.scheme())
        } else {
            format!("{}://{host}:{}", self.scheme(), self.port)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_apps_url_is_its_subdomain_under_the_base_domain() {
        let origin = PublicOrigin::new("example.com", 3000);
        assert_eq!(origin.host_for("blog"), "blog.example.com");
        assert_eq!(origin.url_for("blog"), "http://blog.example.com:3000");
    }

    #[test]
    fn a_default_port_is_left_out_of_the_url() {
        // Both defaults, because a deployment on 80 is as real as one on 443 and
        // `http://blog.example.com:80` is nobody's idea of a URL to paste.
        assert_eq!(
            PublicOrigin::new("example.com", 80).url_for("blog"),
            "http://blog.example.com"
        );
        assert_eq!(
            PublicOrigin::new("example.com", 443)
                .secure(true)
                .url_for("blog"),
            "https://blog.example.com"
        );
        // A non-default port survives, on either scheme.
        assert_eq!(
            PublicOrigin::new("example.com", 8443)
                .secure(true)
                .url_for("blog"),
            "https://blog.example.com:8443"
        );
    }
}
