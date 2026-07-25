//! Who is writing, and what led them here (§7.3, §10.2).
//!
//! A [`CallerContext`] travels with **every** row write, not only with the ones
//! on an RLS table. It started as the RLS half alone — the role and user an
//! `sc.role`/`sc.user` GUC carries into a policy — and grew the rest when table
//! events landed (Phase 4): an event has to say *who* caused the write, and the
//! row layer has no other way to know. One carrier rather than two, because a
//! second parameter threaded through the same eight entry points would be a
//! second thing to forget.
//!
//! So it answers three questions about one write:
//!
//! - **Whose authority?** [`role`](CallerContext::role) — what the policies gate
//!   on, and what an event reports.
//! - **Which user?** [`user`](CallerContext::user) — the fields the formula
//!   language binds `user` to, and the object the `sc.user` GUC carries.
//! - **What led here?** [`chain`](CallerContext::chain) — the trigger names, if
//!   any, whose actions caused this write. RLS ignores it; the dispatcher uses it
//!   to bound a cascade (`Event::firing`, §10.2's `MAX_DEPTH`). A write made by a
//!   request rather than by a trigger carries an empty chain, which is what makes
//!   depth 0 the ordinary case.

use serde_json::Value as Json;

/// The caller one row operation runs as.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CallerContext {
    /// The caller's role (public when anonymous).
    pub role: u8,
    /// The logged-in user's fields as a JSON object, or `None` when anonymous —
    /// in which case `sc.user` is left unset and the policies'
    /// `current_setting('sc.user', true)` reads `NULL`.
    pub user: Option<Json>,
    /// The trigger names whose actions led to this write, outermost first. Empty
    /// for a write a request made directly.
    pub chain: Vec<String>,
}

impl CallerContext {
    /// A context for `role` carrying `user`'s fields.
    pub fn new(role: u8, user: Option<Json>) -> CallerContext {
        CallerContext {
            role,
            user,
            chain: Vec::new(),
        }
    }

    /// A context for `role` with no user (anonymous).
    pub fn anonymous(role: u8) -> CallerContext {
        CallerContext::new(role, None)
    }

    /// The chain of triggers this write descends from — what
    /// `Event::firing` handed the action making it.
    pub fn chained(mut self, chain: Vec<String>) -> CallerContext {
        self.chain = chain;
        self
    }

    /// The user's fields as the JSON text the `sc.user` GUC carries.
    pub(crate) fn user_json(&self) -> Option<String> {
        self.user.as_ref().map(Json::to_string)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_caller_carries_its_role_its_user_and_what_led_here() {
        let anonymous = CallerContext::anonymous(100);
        assert_eq!(anonymous.role, 100);
        assert!(anonymous.user.is_none());
        // Nothing led here: the ordinary case is a request, at depth 0.
        assert!(anonymous.chain.is_empty());
        assert_eq!(anonymous.user_json(), None);

        let user = json!({ "id": 1, "email": "a@b.c" });
        let caller = CallerContext::new(1, Some(user.clone())).chained(vec!["audit".into()]);
        assert_eq!(caller.chain, vec!["audit".to_owned()]);
        // The GUC carries the object as compact JSON — what `UserEnv::Guc`'s
        // `current_setting('sc.user', …)::jsonb ->> 'x'` reads.
        assert_eq!(
            caller.user_json().as_deref(),
            Some(user.to_string().as_str())
        );
    }
}
