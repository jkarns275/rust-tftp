use error::TFTPError;
use std::fmt::Debug;

pub type RawResponse<'a> = &'a [u8];
pub type RawRequest = Vec<u8>;

pub type TFTPResult<T: Debug> = Result<T, TFTPError>;
