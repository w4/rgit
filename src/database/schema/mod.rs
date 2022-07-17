use sled::IVec;
use yoke::Yoke;

pub mod commit;
pub mod prefixes;
pub mod repository;

pub type Yoked<T> = Yoke<T, Box<IVec>>;
