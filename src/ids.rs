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
