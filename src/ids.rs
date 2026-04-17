// Typed identity newtypes for the remote-sync layer.
//
// Scope (per `docs/rust-type-safety-plan.md` Node 3): these newtypes
// wrap the strings that already exist on the wire — `Session.id`,
// `Project.id`, `RemoteConfig.id`, etc. — so the *internal* mapping
// code that crosses the local / remote boundary cannot accidentally
// substitute the wrong identity space. The wire types in `src/wire.rs`
// stay on `String` for two reasons:
//
// 1. Downstream consumers (TypeScript, tests, HTTP clients) have
//    already calibrated to string ids; flipping the wire breaks them.
// 2. Inbound remote payloads land as raw strings via `serde_json`;
//    the conversion point is the remote-sync layer, which now takes
//    typed ids as function arguments.
//
// All the newtypes are `#[serde(transparent)]` so they serialize and
// deserialize identically to their inner `String` — this lets a field
// of type `RemoteSessionId` appear alongside `String`-shaped wire
// fields without breaking the JSON surface.
//
// Borrow shape:
//
// - `as_str(&self) -> &str` for the common "I need a borrowed &str"
//   case (HTTP path builders, URL encoders, Display args).
// - `impl AsRef<str>` so `HashMap<_, _>::get` / similar lookup APIs
//   accept these ids where they historically took `&str` via
//   `impl Borrow<str> for String`.
// - `impl Display` so `format!` / `{}` interpolation works without
//   explicit `.as_str()`.
// - `From<String>` and `From<&str>` for convenience at the
//   serde/wire boundary.
// - `into_inner(self) -> String` when we need to hand the underlying
//   string to an API that owns it (e.g. HashMap keys).
//
// Deliberately NOT implementing `Deref<Target = str>`: a `&LocalSessionId`
// and a `&RemoteSessionId` stay as distinct types at function signatures,
// which is the whole point of the exercise. Deref would let `&*id` work
// for either, which is ergonomic but weakens the type-level contract at
// call sites that currently rely on `&str` conventions.

macro_rules! define_typed_id {
    ($name:ident, $doc:expr) => {
        #[doc = $doc]
        // `allow(dead_code)` at the struct level is intentional: the
        // newtypes in this file are a type-safety vocabulary shared
        // across future migrations, and not every one is used today
        // (notably `RemoteSessionId` and `RemoteId` — available for
        // the next migration wave). Removing unused types now would
        // thrash reviewers when they come back later; silencing the
        // lint keeps them visible as a named-type menu.
        #[allow(dead_code)]
        #[derive(Clone, Debug, PartialEq, Eq, Hash, Deserialize, Serialize)]
        #[serde(transparent)]
        struct $name(String);

        #[allow(dead_code)]
        impl $name {
            fn as_str(&self) -> &str {
                &self.0
            }

            fn into_inner(self) -> String {
                self.0
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_owned())
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        // `Borrow<str>` lets `HashMap<$name, V>::get(key_str)` work
        // without allocating a temporary `$name` wrapper at the call
        // site. Safe because `String`'s derived `Hash` / `Eq` route
        // through its `&str` deref, so a borrowed `str` hashes and
        // compares identically to the equivalent owned `$name(...)`
        // key — satisfying the `Borrow` contract that borrow-form
        // equality agrees with owned-form equality.
        impl std::borrow::Borrow<str> for $name {
            fn borrow(&self) -> &str {
                &self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(f)
            }
        }
    };
}

define_typed_id!(
    LocalSessionId,
    "TermAl-side session id — the value stored in `Session.id` for
every locally-known session, including remote-proxy mirrors."
);

define_typed_id!(
    RemoteSessionId,
    "Session id as emitted by a remote TermAl backend — the value
stored in `SessionRecord.remote_session_id` and carried on inbound
remote payloads."
);

define_typed_id!(
    LocalProjectId,
    "TermAl-side project id — the value stored in `Project.id` for
every locally-known project, including remote-proxy project mirrors."
);

define_typed_id!(
    RemoteProjectId,
    "Project id as emitted by a remote TermAl backend — the value
stored in `Project.remote_project_id` and carried on inbound remote
`Session.project_id` fields."
);

define_typed_id!(
    RemoteId,
    "Remote-host id — `RemoteConfig.id`, used as the key in
`StateInner.remote_applied_revisions` and as the scope namespace for
every `(RemoteId, RemoteSessionId)` pair."
);

#[cfg(test)]
mod ids_tests {
    // Round-trip tests covering the macro-generated impls. The core
    // invariant these pin: `#[serde(transparent)]` means each newtype
    // serializes and deserializes wire-identically to its inner
    // `String`, so TypeScript consumers and inbound remote payloads
    // stay on bare strings. If someone drops `#[serde(transparent)]`
    // from `define_typed_id!` (e.g., to add a validation hook), JSON
    // would suddenly serialize `{ "0": "abc" }` instead of `"abc"`,
    // breaking every `Session.id` / `Project.id` field on the wire.
    // These tests fire before the break ships.
    use super::*;
    use std::borrow::Borrow;
    use std::collections::HashMap;

    #[test]
    fn local_session_id_round_trips_as_bare_string() {
        let id = LocalSessionId::from("session-7");
        let json = serde_json::to_string(&id).expect("serialize LocalSessionId");
        assert_eq!(json, "\"session-7\"");
        let parsed: LocalSessionId =
            serde_json::from_str(&json).expect("deserialize LocalSessionId");
        assert_eq!(parsed, id);
    }

    #[test]
    fn remote_session_id_round_trips_as_bare_string() {
        let id = RemoteSessionId::from("remote-session-42");
        let json = serde_json::to_string(&id).expect("serialize RemoteSessionId");
        assert_eq!(json, "\"remote-session-42\"");
        let parsed: RemoteSessionId =
            serde_json::from_str(&json).expect("deserialize RemoteSessionId");
        assert_eq!(parsed.as_str(), "remote-session-42");
    }

    #[test]
    fn local_project_id_round_trips_as_bare_string() {
        let id = LocalProjectId::from("project-local");
        let json = serde_json::to_string(&id).expect("serialize LocalProjectId");
        assert_eq!(json, "\"project-local\"");
        let parsed: LocalProjectId =
            serde_json::from_str(&json).expect("deserialize LocalProjectId");
        assert_eq!(parsed, id);
    }

    #[test]
    fn remote_project_id_round_trips_as_bare_string() {
        let id = RemoteProjectId::from("project-remote");
        let json = serde_json::to_string(&id).expect("serialize RemoteProjectId");
        assert_eq!(json, "\"project-remote\"");
        let parsed: RemoteProjectId =
            serde_json::from_str(&json).expect("deserialize RemoteProjectId");
        assert_eq!(parsed.as_str(), "project-remote");
    }

    #[test]
    fn remote_id_round_trips_as_bare_string() {
        let id = RemoteId::from("remote-ssh-1");
        let json = serde_json::to_string(&id).expect("serialize RemoteId");
        assert_eq!(json, "\"remote-ssh-1\"");
        let parsed: RemoteId = serde_json::from_str(&json).expect("deserialize RemoteId");
        assert_eq!(parsed.as_str(), "remote-ssh-1");
    }

    #[test]
    fn transparent_means_the_wire_is_a_bare_string_not_an_object() {
        // Negative case: if anyone drops `#[serde(transparent)]` from
        // the macro, Serde falls back to struct-style serialization,
        // emitting `{"inner":"..."}` / `{"0":"..."}` / similar. The
        // whole downstream contract (TypeScript, Python clients, the
        // `Session.id: String` field on the wire) depends on this
        // staying a bare string.
        let id = LocalSessionId::from("wire-sanity-check");
        let json = serde_json::to_string(&id).expect("serialize");
        assert!(
            !json.contains(':') && !json.contains('{'),
            "expected a bare JSON string, got {json}"
        );
    }

    #[test]
    fn display_renders_the_inner_string_without_decoration() {
        // `Display` is used for log/tracing/format!-interpolation
        // output throughout the remote-sync layer. If this grows a
        // wrapper like "LocalSessionId(abc)" it'd change every log
        // line and every HTTP path constructed via `format!`.
        assert_eq!(format!("{}", LocalSessionId::from("alpha")), "alpha");
        assert_eq!(format!("{}", RemoteSessionId::from("beta")), "beta");
        assert_eq!(format!("{}", LocalProjectId::from("gamma")), "gamma");
        assert_eq!(format!("{}", RemoteProjectId::from("delta")), "delta");
        assert_eq!(format!("{}", RemoteId::from("epsilon")), "epsilon");
    }

    #[test]
    fn as_ref_str_and_borrow_str_both_yield_the_inner_slice() {
        let id = LocalProjectId::from("needle");
        assert_eq!(<LocalProjectId as AsRef<str>>::as_ref(&id), "needle");
        assert_eq!(<LocalProjectId as Borrow<str>>::borrow(&id), "needle");
    }

    #[test]
    fn into_inner_returns_the_owned_string_without_reallocation() {
        let id = LocalSessionId::from(String::from("owned"));
        // `.into_inner()` takes `self` — the compiler would refuse a
        // stale reuse of `id` below if this ever changed to borrow.
        let owned: String = id.into_inner();
        assert_eq!(owned, "owned");
    }

    #[test]
    fn from_string_consumes_the_owned_string() {
        // `From<String>` is the hot-path constructor used in
        // `local_session_id_for_remote_session` to avoid a double
        // allocation; the test pins it as available and correct.
        let owned = String::from("via-from-string");
        let id = LocalSessionId::from(owned);
        assert_eq!(id.as_str(), "via-from-string");
    }

    #[test]
    fn from_str_allocates_a_fresh_owned_string() {
        // Convenience constructor for borrow-side callers. Equivalent
        // result to `From<String>` but copies.
        let id = LocalSessionId::from("via-from-str");
        assert_eq!(id.as_str(), "via-from-str");
    }

    #[test]
    fn borrow_str_enables_hashmap_lookup_without_wrapper_allocation() {
        // This is THE reason `Borrow<str>` exists on the typed ids:
        // `HashMap<LocalProjectId, _>::get("some &str")` must work
        // without building a temporary `LocalProjectId::from(...)` at
        // the call site. The hot remote-sync localization path
        // depends on this avoided allocation.
        let mut map: HashMap<LocalProjectId, i32> = HashMap::new();
        map.insert(LocalProjectId::from("alpha"), 1);
        map.insert(LocalProjectId::from("beta"), 2);
        assert_eq!(map.get("alpha").copied(), Some(1));
        assert_eq!(map.get("beta").copied(), Some(2));
        assert_eq!(map.get("gamma"), None);
        // Confirms the contract: borrow-form hashes and equals the
        // owned form, so keys resolve correctly.
    }

    #[test]
    fn equality_and_hash_agree_between_borrow_and_owned_forms() {
        // Formal statement of the `Borrow` contract the previous test
        // relies on: if two values compare equal in borrowed form,
        // they must hash identically, and their owned forms must
        // likewise be equal. A regression to a newtype with a custom
        // `Hash` that bakes in the type name would break this.
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let owned = RemoteProjectId::from("same");
        let borrowed: &str = "same";

        // Hash via the newtype
        let mut hasher_a = DefaultHasher::new();
        owned.hash(&mut hasher_a);
        // Hash via &str directly
        let mut hasher_b = DefaultHasher::new();
        borrowed.hash(&mut hasher_b);
        assert_eq!(
            hasher_a.finish(),
            hasher_b.finish(),
            "Borrow<str> contract requires identical hashes"
        );

        // Equality via Borrow is consistent
        assert_eq!(<RemoteProjectId as Borrow<str>>::borrow(&owned), borrowed);
    }

    #[test]
    fn clone_and_debug_are_derived_and_produce_distinct_but_equal_instances() {
        // Sanity check on the rest of the derive set — `Clone` is used
        // by `HashMap::get(...).cloned()` in the remote-sync lookups,
        // `Debug` by `assert_eq!` failure rendering.
        let original = RemoteId::from("debug-me");
        let cloned = original.clone();
        assert_eq!(original, cloned);
        let debug = format!("{cloned:?}");
        assert!(debug.contains("debug-me"));
    }
}
