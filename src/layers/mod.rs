use std::convert::Infallible;

pub mod logger;

pub trait UnwrapInfallible<T> {
    fn unwrap_infallible(self) -> T;
}

impl<T> UnwrapInfallible<T> for Result<T, Infallible> {
    fn unwrap_infallible(self) -> T {
        self.unwrap()
    }
}

impl<T> UnwrapInfallible<T> for Result<T, &Infallible> {
    fn unwrap_infallible(self) -> T {
        self.unwrap()
    }
}
