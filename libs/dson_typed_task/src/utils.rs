pub mod prelude {
    pub use super::*;
}

pub struct StrErr {
    pub message: String,
}
impl std::error::Error for StrErr {}
impl std::fmt::Display for StrErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}
impl std::fmt::Debug for StrErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}
impl From<String> for StrErr {
    fn from(s: String) -> Self {
        StrErr { message: s }
    }
}
impl From<&str> for StrErr {
    fn from(s: &str) -> Self {
        StrErr {
            message: s.to_string(),
        }
    }
}
impl From<std::io::Error> for StrErr {
    fn from(e: std::io::Error) -> Self {
        StrErr {
            message: e.to_string(),
        }
    }
}

pub trait ErrContext {
    fn context(self, message: &str) -> StrErr;
}
impl<E: std::error::Error> ErrContext for E {
    fn context(self, message: &str) -> StrErr {
        StrErr::from(format!("{}: {}", message, self))
    }
}

pub trait ResultExt {
    type Ok;
    fn context(self, message: &str) -> Result<Self::Ok, StrErr>;
}
impl<T, E: std::fmt::Display> ResultExt for Result<T, E> {
    type Ok = T;
    fn context(self, message: &str) -> Result<T, StrErr> {
        self.map_err(|e| StrErr::from(format!("{}: {}", message, e)))
    }
}
