//! Module doc
use std::collections::HashMap;
use std::io;
use std::{fs, net};

const MAX: usize = 1024;
static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A configuration.
#[derive(Debug, Clone)]
pub struct Config {
    pub name: String,
    pub port: u16,
}

enum Color {
    Red,
    Green,
}

pub type Result<T> = std::result::Result<T, MyError>;

pub trait Handler {
    fn handle(&self, req: Request) -> Response;
}

impl Config {
    pub fn new(name: String) -> Self {
        todo!()
    }
    fn validate(&self) -> bool {
        true
    }
}

pub fn process(input: &str) -> Result<String> {
    todo!()
}

pub mod utils;
mod internal;

macro_rules! my_macro {
    () => {};
}

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {}
}
