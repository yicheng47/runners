#![allow(dead_code)] // Variants land in C1 for consumption across C2+.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("sqlite pool: {0}")]
    Pool(#[from] r2d2::Error),

    #[error("json: {0}")]
    Json(#[from] serde_json::Error),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("{0}")]
    Msg(String),
}

impl Error {
    pub fn msg(s: impl Into<String>) -> Self {
        Error::Msg(s.into())
    }
}

// Lift shared-core errors into the app error so command code can `?` across
// the boundary. We flatten to existing variants rather than adding a `Core`
// wrapper that'd need its own Display.
impl From<runner_core::Error> for Error {
    fn from(e: runner_core::Error) -> Self {
        match e {
            runner_core::Error::Io(err) => Error::Io(err),
            runner_core::Error::Json(err) => Error::Json(err),
            runner_core::Error::Msg(s) => Error::Msg(s),
        }
    }
}

impl serde::Serialize for Error {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
