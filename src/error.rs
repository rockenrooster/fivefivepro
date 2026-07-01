use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::io;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pro55Error {
    message: String,
}

impl Pro55Error {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl Display for Pro55Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for Pro55Error {}

impl From<io::Error> for Pro55Error {
    fn from(value: io::Error) -> Self {
        Self::new(value.to_string())
    }
}

impl From<std::num::ParseIntError> for Pro55Error {
    fn from(value: std::num::ParseIntError) -> Self {
        Self::new(value.to_string())
    }
}

impl From<std::string::FromUtf8Error> for Pro55Error {
    fn from(value: std::string::FromUtf8Error) -> Self {
        Self::new(value.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Pro55Error>;
