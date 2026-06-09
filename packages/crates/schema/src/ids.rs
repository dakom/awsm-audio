//! Stable identifiers used throughout the schema.
//!
//! [`NodeId`] / [`SampleId`] / [`AssetId`] are UUIDs (collision-free, editor can
//! mint them client-side). [`PortId`] and [`ParamHandle`] are author-chosen
//! names — they're part of a sample's public surface (its inlets/outlets and
//! macro knobs), so a human-readable string is the right identity.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! uuid_id {
    ($(#[$m:meta])* $name:ident) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub Uuid);

        impl $name {
            /// Mint a fresh random id.
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }
        }

        // A fresh random id is the only sensible "default"; never the nil UUID,
        // which would collide across every default-constructed value.
        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(f)
            }
        }

        impl std::str::FromStr for $name {
            type Err = uuid::Error;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Ok(Self(s.parse()?))
            }
        }

        // On the wire a uuid id is a UUID string; describe it as such for JSON
        // Schema (the MCP server's typed tool params) rather than recursing into
        // Uuid (which would need schemars' uuid1 feature).
        #[cfg(feature = "schemars")]
        impl schemars::JsonSchema for $name {
            fn schema_name() -> std::borrow::Cow<'static, str> {
                stringify!($name).into()
            }
            fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
                schemars::json_schema!({ "type": "string", "format": "uuid" })
            }
        }
    };
}

uuid_id!(
    /// Identifies a [`Node`](crate::Node) instance within a single
    /// [`Graph`](crate::Graph).
    NodeId
);
uuid_id!(
    /// Identifies a [`Sample`](crate::Sample) within a
    /// [`SampleLibrary`](crate::SampleLibrary).
    SampleId
);
uuid_id!(
    /// Identifies a shared asset (buffer, periodic wave, curve) in the
    /// [`AssetTable`](crate::AssetTable).
    AssetId
);

macro_rules! name_id {
    ($(#[$m:meta])* $name:ident) => {
        $(#[$m])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl<T: Into<String>> From<T> for $name {
            fn from(s: T) -> Self {
                Self(s.into())
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }

        // Author-chosen name id: a plain string on the wire.
        #[cfg(feature = "schemars")]
        impl schemars::JsonSchema for $name {
            fn schema_name() -> std::borrow::Cow<'static, str> {
                stringify!($name).into()
            }
            fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
                schemars::json_schema!({ "type": "string" })
            }
        }
    };
}

name_id!(
    /// Author-facing name of a sample inlet/outlet (boundary audio port).
    PortId
);
name_id!(
    /// Author-facing name of a sample's exposed macro parameter.
    ParamHandle
);
