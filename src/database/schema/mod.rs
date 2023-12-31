#![allow(clippy::module_name_repetitions)]

use sled::IVec;
use yoke::Yoke;

pub mod commit;
pub mod prefixes;
pub mod repository;
pub mod tag;

pub type Yoked<T> = Yoke<T, Box<IVec>>;

pub const SCHEMA_VERSION: &str = "1";
