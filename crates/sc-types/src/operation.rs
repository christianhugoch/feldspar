//! [`Operation`]: an *act* a configurable extension offers, declared as data —
//! the counterpart to [`FormField`](crate::FormField), which declares its
//! *settings*.
//!
//! ## Why this exists
//!
//! §6.2's "settings as data" makes the admin UI able to configure an extension
//! it knows nothing about: the extension declares `Vec<FormField>` and the UI
//! renders whatever it is handed. That covers every extension whose whole
//! configuration is values in a bag.
//!
//! Some extensions also offer things to *do*, and a settings spec cannot express
//! one. A git-backed file store must be able to generate a deploy key before it
//! is saved, and to pull, push and commit afterwards; none of those is a value
//! the admin types. Without a vocabulary for them the admin UI would need a
//! branch per extension — `if backend === "git"` — which is exactly the coupling
//! `FormField` exists to remove, and which would make an operation something
//! only a *built-in* extension could have. A plugin backend supplied through
//! `sc-code` could declare settings and never a button.
//!
//! So an operation is declared the same way a setting is: a name, a label, when
//! it can be run, and — since some operations need an argument — its own
//! `Vec<FormField>` for whatever it asks the admin for. The UI renders a button
//! per operation and a form per argument, and knows nothing about any of them.
//!
//! ## What an operation may do
//!
//! Running one produces two things, and both are deliberately generic:
//!
//! - **Output**: text to show the admin. A command's own output, a summary, a
//!   "nothing to do". Text rather than a structure because the useful part is
//!   usually a subprocess's own words, and parsing those into a vocabulary of
//!   ours would be inventing one.
//! - **Changes to the definition it ran against.** An operation is handed the
//!   extension's configuration mutably; a deploy-key generator fills in two
//!   settings, a clone records where it cloned to. The caller persists whatever
//!   changed. This is what lets an operation *configure* rather than only act.

use crate::FormField;

/// When an [`Operation`] can be run, which decides both what it is handed and
/// where the admin UI offers it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationScope {
    /// Runs against configuration that is being **edited and not yet saved** —
    /// so it is offered on the create form, and what it changes goes back into
    /// the form rather than to the database.
    ///
    /// This is the scope that makes "generate a deploy key" possible at all.
    /// The key must exist before the store is saved, because saving a git store
    /// clones it and cloning needs a key the remote already accepts; an
    /// operation that required a saved store would always be one step too late.
    Configure,
    /// Runs against a **saved** instance. Offered when editing an existing one,
    /// and anything it changes is persisted.
    Instance,
}

/// One act an extension offers, beyond the settings it declares (see the module
/// docs).
#[derive(Debug, Clone, PartialEq)]
pub struct Operation {
    /// The identifier the operation is invoked by — part of the API surface, so
    /// it is a stable name rather than a position.
    pub name: String,
    /// The button's text.
    pub label: String,
    /// A sentence explaining what running it does, shown beside the button.
    /// Empty when the label says it all.
    pub description: String,
    /// When it can be run, and therefore where it is offered.
    pub scope: OperationScope,
    /// What it asks the admin for — a commit message, a tag name. Rendered by
    /// the same code that renders settings, because it is the same vocabulary.
    pub input_spec: Vec<FormField>,
    /// Whether this operation is part of **creating** an instance: run once,
    /// when it is first saved, and the creation fails if it does.
    ///
    /// For the operation that brings the thing being configured into existence
    /// — a git store's clone, an object store's bucket. It is what makes a
    /// creation *transactional*: nothing is persisted unless it succeeded, so a
    /// failed attempt leaves no half-made instance behind for the admin's second
    /// attempt to collide with. Without it the admin fixes the setting that was
    /// wrong, presses the button again, and is told the name is already taken —
    /// by the very row their failed attempt left.
    ///
    /// It is an ordinary operation as well, and stays on the button list: an
    /// instance can lose what was created for it (a working copy deleted, a
    /// database restored onto a fresh machine), and running it again is the
    /// repair.
    pub on_create: bool,
    /// Whether to run it **as soon as the screen opens**, rather than waiting
    /// for a button.
    ///
    /// For the operation whose whole purpose is to report state — "what does
    /// this repository look like right now?" — which an admin needs in front of
    /// them before deciding what to do, not after pressing something. Only
    /// meaningful for [`Instance`](OperationScope::Instance) scope, and only
    /// appropriate for an operation that changes nothing.
    pub automatic: bool,
}

impl Operation {
    /// An operation of the given name and scope: labelled after its name, with
    /// no description, no input and not automatic.
    pub fn new(name: impl Into<String>, scope: OperationScope) -> Operation {
        let name = name.into();
        Operation {
            label: name.clone(),
            name,
            description: String::new(),
            scope,
            input_spec: Vec::new(),
            on_create: false,
            automatic: false,
        }
    }

    /// Set the button text.
    pub fn label(mut self, label: impl Into<String>) -> Operation {
        self.label = label.into();
        self
    }

    /// Set the explanatory sentence.
    pub fn description(mut self, description: impl Into<String>) -> Operation {
        self.description = description.into();
        self
    }

    /// Declare what the operation asks the admin for.
    pub fn input(mut self, spec: impl IntoIterator<Item = FormField>) -> Operation {
        self.input_spec = spec.into_iter().collect();
        self
    }

    /// Mark it as part of creating an instance: it runs on the first save, and
    /// a failure means nothing is created.
    pub fn on_create(mut self) -> Operation {
        self.on_create = true;
        self
    }

    /// Run it when the screen opens rather than on a button press.
    pub fn automatic(mut self) -> Operation {
        self.automatic = true;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BasicType;

    #[test]
    fn an_operation_defaults_to_its_name_and_nothing_else() {
        let op = Operation::new("pull", OperationScope::Instance);
        assert_eq!(op.name, "pull");
        assert_eq!(op.label, "pull");
        assert_eq!(op.description, "");
        assert!(op.input_spec.is_empty());
        assert!(!op.automatic);
        assert!(!op.on_create);
    }

    #[test]
    fn the_setters_chain() {
        let op = Operation::new("commit", OperationScope::Instance)
            .label("Commit all changes")
            .description("Stages and commits everything in the working tree.")
            .input([
                FormField::new("message", BasicType::Text)
                    .label("Commit message")
                    .required(),
            ]);
        assert_eq!(op.label, "Commit all changes");
        assert!(op.description.starts_with("Stages"));
        // The argument is an ordinary `FormField`, so the UI renders it with the
        // same code that renders settings.
        assert_eq!(op.input_spec.len(), 1);
        assert_eq!(op.input_spec[0].name(), "message");
        assert!(op.input_spec[0].required);
    }

    #[test]
    fn a_creation_operation_is_still_an_ordinary_one() {
        // It runs on the first save *and* stays on the button list, because an
        // instance can lose what was created for it and running it again is the
        // repair.
        let op = Operation::new("clone", OperationScope::Instance).on_create();
        assert!(op.on_create);
        assert!(!op.automatic);
    }

    #[test]
    fn an_automatic_operation_reports_rather_than_acts() {
        let op = Operation::new("status", OperationScope::Instance).automatic();
        assert!(op.automatic);
        assert_eq!(op.scope, OperationScope::Instance);
    }
}
