//! MJML as an HTML body: compiling the markup an email designer writes into the
//! markup a mail client will lay out (design §18.2).
//!
//! An HTML email is not a web page. There is no flexbox, no `max-width` that
//! Outlook honours and no stylesheet that Gmail keeps whole, so a message that
//! renders the same in ten clients is a nest of tables, `mso` conditional
//! comments and inlined attributes that nobody writes by hand.
//! [MJML](https://mjml.io) is the language that exists to be *compiled into*
//! that: `mj-section`/`mj-column`/`mj-button` in, the tables out.
//!
//! This is one function because that is all it is. The interpolation happens
//! **first** — the `{{ }}` tokens are rendered into the MJML source, escaped by
//! the ordinary HTML rule, so a customer whose name contains `<` cannot inject
//! markup — and the result is then compiled. Doing it the other way would mean
//! interpolating into generated tables, where an escaped value could land inside
//! an attribute or a conditional comment and the template author has no way to
//! see it.
//!
//! **Compiled in process.** v1 renders MJML by shelling out to the language's
//! own JavaScript implementation; [`mrml`] is a reimplementation in Rust, so
//! this is a function call and a deployment needs no Node. The parser's default
//! include loader refuses `mj-include`, which is the behaviour to want: the
//! template is data an admin typed, and it must not be able to make the server
//! read a file or fetch a URL.

use mrml::prelude::render::RenderOptions;
use sc_error::{Error, Result};

/// Compile an MJML document into the HTML that goes in the message.
///
/// Both failures — markup that does not parse and a document that parses but
/// cannot be rendered — are `invalid` rather than internal: the source is what
/// an admin typed into the trigger's form, and mrml's own message names the
/// element and the position, which is the only part of this a person can act on.
pub fn render_mjml(source: &str) -> Result<String> {
    let parsed = mrml::parse(source)
        .map_err(|e| Error::invalid(format!("the MJML body could not be parsed: {e}")))?;
    parsed
        .element
        .render(&RenderOptions::default())
        .map_err(|e| Error::invalid(format!("the MJML body could not be rendered: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_document_compiles_to_the_html_a_mail_client_lays_out() {
        let html = render_mjml(
            "<mjml><mj-body><mj-section><mj-column>\
             <mj-text>Receipt for order 42</mj-text>\
             </mj-column></mj-section></mj-body></mjml>",
        )
        .unwrap();
        // The text survives, and the layout it arrives in is the table markup
        // that is the whole point of compiling rather than sending the source.
        assert!(html.contains("Receipt for order 42"), "{html}");
        assert!(html.contains("<table"), "{html}");
        assert!(html.to_lowercase().contains("<!doctype html"), "{html}");
    }

    #[test]
    fn markup_that_is_not_mjml_is_refused_by_name() {
        // A body the admin forgot to write as MJML: it has no `<mjml>` root, so
        // it is a mistake worth naming rather than an empty message.
        let err = render_mjml("<p>hello</p>").unwrap_err().to_string();
        assert!(err.contains("MJML"), "{err}");

        // An unclosed element, which is the everyday typo.
        let err = render_mjml("<mjml><mj-body><mj-section></mjml>")
            .unwrap_err()
            .to_string();
        assert!(err.contains("MJML"), "{err}");
    }

    /// `mj-include` is refused, because a template is data an admin typed and it
    /// must not be able to reach the filesystem or the network.
    #[test]
    fn an_include_cannot_reach_out_of_the_document() {
        let err =
            render_mjml("<mjml><mj-body><mj-include path=\"/etc/passwd\" /></mj-body></mjml>")
                .unwrap_err()
                .to_string();
        assert!(err.contains("MJML"), "{err}");
    }
}
