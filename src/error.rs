use std::io;
use std::string::FromUtf8Error;

#[derive(Debug)]
pub enum TFTPError {
    /// An i/o error occurred.
    IOError(io::Error),

    /// The opcode in received header was invalid (valid values are 1-5)
    InvalidOpcode(u16),

    /// The specified filename is invalid. This will happen in the event that a filename
    /// contains an invalid character, or if the filename string is not null terminated.
    InvalidFilename(Box<[u8]>),

    /// The specified filename was the empty string, which is not valid.
    EmptyFilename,

    /// The specified mode (in a RRQ or WRQ) was empty. Valid values are:
    ///
    /// "mail"
    /// "netascii"
    /// "octet"
    ///
    /// in any letter case (upper or lower, or any combination).
    EmptyMode,

    /// The specified mode (in a RRQ or WRQ) was invalid. Valid values are:
    ///
    /// "mail",
    /// "netascii",
    /// "octet"
    ///
    /// in any letter case case (upper or lower, or any combination).
    ///
    /// If the string in the header is not null-terminated, this may occur.
    InvalidMode(Box<[u8]>),

    /// The header was too small to parse
    InvalidHeaderLen,

    /// The data packet is too short; it contains only a header with no data.
    InvalidDataLen,

    /// A string that was to placed into a header contains a null (0) character, which is not valid.
    InvalidString,

    /// The UDP connection suddenly closed
    ConnectionClosed,

    /// Received data from the wrong source address
    WrongHost,

    /// A string in a header contained invalid unicode.
    InvalidUnicodeString(FromUtf8Error)
}