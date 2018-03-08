use error::TFTPError;
use std::cmp;
use types::*;
use std::mem;
use std::marker::PhantomData;
use std::ascii::AsciiExt;

const OPCODE_RRQ: u8 = 1;
const OPCODE_WRQ: u8 = 2;
const OPCODE_DATA: u8 = 3;
const OPCODE_ACK: u8 = 4;
const OPCODE_ERROR: u8 = 5;

/// RFC1350 specifies 3 RW modes. As of right now, Mail functionality will be left out.
#[derive(Clone, Copy, Debug)]
pub enum RWMode {
    /// The filename is a email address or username; the data is the body of the email.
    Mail,

    /// Translate received data to the endianness of the current machine.
    NetASCII,

    /// Leave the data as it is.
    Octet,
}

impl RWMode {
    fn from_str<S: AsRef<str>>(src: S) -> Option<RWMode> {
        let p = src.as_ref().to_owned();
        match p.to_lowercase().as_ref() {
            "mail" => Some(RWMode::Mail),
            "netascii" => Some(RWMode::NetASCII),
            "octet" => Some(RWMode::Octet),
            _ => None
        }
    }
}

impl Into<String> for RWMode {
    fn into(self) -> String {
        match self {
            RWMode::Mail => "mail".to_string(),
            RWMode::NetASCII => "netascii".to_string(),
            RWMode::Octet => "octet".to_string()
        }
    }
}

impl Into<&'static [u8]> for RWMode {
    fn into(self) -> &'static [u8] {
        match self {
            RWMode::Mail => "mail".as_ref(),
            RWMode::NetASCII => "netascii".as_ref(),
            RWMode::Octet => "octet".as_ref()
        }
    }
}

#[derive(Clone, Debug)]
#[repr(u8)]
pub enum RequestType {
    Write = OPCODE_WRQ,
    Read = OPCODE_RRQ
}

pub trait ToRequestType {
    fn request_type() -> RequestType;
}

pub struct ReadRequest;
impl ToRequestType for ReadRequest {
    fn request_type() -> RequestType { RequestType::Read }
}

pub struct WriteRequest;
impl ToRequestType for WriteRequest {
    fn request_type() -> RequestType { RequestType::Write }
}

/// Represents either a ReadRequest or a WriteRequest; in any case, the raw format is as follows:
/// ```text
///        2 bytes    string   1 byte     string   1 byte
///        -----------------------------------------------
/// RRQ/  | 01/02 |  Filename  |   0  |    Mode    |   0  |
/// WRQ    -----------------------------------------------
/// ```
/// Note: all strings in headers are null-terminated c-style strings, hence the 0 after both strings
#[derive(Clone, Debug)]
pub struct RWHeader<T: ToRequestType> {
    /// The name / path of the file to be read / written.
    pub filename: String,

    /// The mode of data transfer
    pub mode: RWMode,

    _pd: PhantomData<T>
}

impl<T: ToRequestType> RWHeader<T> {
    pub fn new(filename: String, mode: RWMode) -> Result<Self, TFTPError> {
        if filename.contains('\0') {
            return Err(TFTPError::InvalidFilename(filename.into_bytes().into_boxed_slice()))
        }

        Ok(RWHeader {
            filename,
            mode,
            _pd: PhantomData
        })
    }

    pub fn into_raw(self) -> RawRequest { self.into() }

    pub fn from_raw(src: RawResponse) -> TFTPResult<Self> {
        // The upper bits of the op # are not used, since the only valid modes are 1 through 5
        debug_assert!(src[0] == 0);
        debug_assert!(src[1] == T::request_type() as u8);
        if src.len() < 6 {
            return Err(TFTPError::InvalidHeaderLen)
        }

        if src[2] == 0 {
            return Err(TFTPError::EmptyFilename)
        }

        let mut filename = Vec::with_capacity(64);
        let mut i = 2;
        loop {
            if src[i] == 0 {
                i += 1;
                break;
            }
            filename.push(src[i].into());
            i += 1;
            if i == src.len() {
                let src_copy = Vec::from(src);
                return Err(TFTPError::InvalidFilename(src_copy.into_boxed_slice()))
            }
        }

        let mut mode = Vec::with_capacity(8);
        loop {
            if src[i] == 0 {
                break;
            } else if src.len() <= i {
                return Err(TFTPError::InvalidMode(Vec::from(src).into_boxed_slice()))
            }
            mode.push(src[i]);
            i += 1;
        }

        if mode.len() == 0 {
            return Err(TFTPError::EmptyMode)
        }

        match (String::from_utf8(filename), String::from_utf8(mode)) {
            (Err(e), _) => Err(TFTPError::InvalidUnicodeString(e)),
            (_, Err(e)) => Err(TFTPError::InvalidUnicodeString(e)),
            (Ok(filename), Ok(mode_string)) => {
                match RWMode::from_str(mode_string) {
                    Some(mode) =>
                        Ok(RWHeader {
                            mode,
                            filename,
                            _pd: PhantomData
                        }),
                    None => Err(TFTPError::InvalidMode(Vec::from(src).into_boxed_slice()))
                }
            }
        }


    }
}

impl<T: ToRequestType> Into<RawRequest> for RWHeader<T> {
    fn into(self) -> RawRequest {
        let mode_slice: &'static [u8] = self.mode.into();
        let len = 4 + self.filename.len() + mode_slice.len();
        let filename: &[u8] = self.filename.as_ref();

        let mut data = vec![0u8; len];
        data[0] = 0;
        data[1] = T::request_type() as u8;

        let mut i = 2;

        data[2..].clone_from_slice(filename);
        i += filename.len();
        data[i] = 0;
        i += 1;

        // Not allowed to have empty string for as a filename
        debug_assert!(data[2] == 0);

        data[i..].clone_from_slice(mode_slice);
        i += mode_slice.len();
        data[i] = 0;
        i += 1;

        data
    }
}

pub const MAX_DATA_LEN: usize = 512;
pub const DATA_HEADER_LEN: usize = 4;

/// Represents a data header; either sent or received.
/// With the exception of the first byte being used as the MSB of the block number to extend the
/// file-size capability of the protocol, this is the format specified by RFC1350:
/// ```text
///        1 byte        1 byte          2 bytes          n bytes
///         -----------------------------------------------------------
///  DATA  | Block # MSB | 0x03 |  Block # lower 2 bytes  |    Data    |
///         -----------------------------------------------------------
/// ```
/// Note: the block # is a 24 bit integer.
#[derive(Clone)]
pub struct DataHeader {

    /// The data of this data of the request. up to MAX_DATA_LEN bytes.
    pub data: [u8; MAX_DATA_LEN],
    /// How many bytes of [data] are actually being used.
    pub data_len: usize,
    /// The block number. Each block is MAX_DATA_LEN bytes in size.
    pub block_number: u32
}

impl DataHeader {

    /// Tries to create a new data header to be sent out.
    /// Returns Some(..) unless block_number * MAX_DATA_LEN goes over the length of data_src.
    pub fn new(mut data_src: &[u8], block_number: u32) -> Option<Self> {
        let index = block_number as usize * MAX_DATA_LEN;
        if data_src.len() <= index {
            None
        } else {
            data_src = &data_src[index..];
            let data_len = cmp::min(data_src.len(), MAX_DATA_LEN);
            let mut data = [0u8; MAX_DATA_LEN];
            data[0..data_len].copy_from_slice(&data_src[..]);
            Some(DataHeader {
                data,
                block_number,
                data_len: data_len
            })
        }
    }

    pub fn into_raw(self) -> RawRequest { self.into() }

    pub fn from_raw(src: RawResponse) -> TFTPResult<Self> {
        debug_assert!(src[1] == OPCODE_DATA);
        if src.len() < 4 {
            return Err(TFTPError::InvalidHeaderLen)
        } else if src.len() < 5 {
            return Err(TFTPError::InvalidDataLen)
        }
        // The MSB of the op# will be used to extend the data # range to 24 bits rather than
        // just the 16 bits as specified by the RFC. The extra byte will be the MSB, so it will not
        // be used unless filesize exceeds MAX_DATA_LEN * 2^16 bytes (~32MB if MAX_DATA_LEN is 512byte)
        let mut block_number = 0u32;
        block_number |= (src[0] as u32) << 16;
        block_number |= (src[2] as u32) << 8;
        block_number |= (src[3] as u32);

        let mut data = [0u8; MAX_DATA_LEN];
        let index = cmp::min(DATA_HEADER_LEN + MAX_DATA_LEN, src.len());
        data[0..index - DATA_HEADER_LEN]
            .copy_from_slice(&src[DATA_HEADER_LEN..index]);
        Ok(DataHeader {
            data,
            block_number,
            data_len: index - DATA_HEADER_LEN
        })
    }
}

impl Into<RawRequest> for DataHeader {
    fn into(self) -> RawRequest {
        let block_number = [(self.block_number >> 16) as u8, (self.block_number >> 8) as u8, (self.block_number) as u8];
        let mut data = vec![0u8; 4 + self.data_len];
        data[1] = OPCODE_ERROR;

        data[0] = block_number[0];
        data[2] = block_number[1];
        data[3] = block_number[2];

        let mut i = 0;


        data[4..].clone_from_slice(&self.data);

        data
    }
}

/// Represents an Acknowledgement header; either sent or received.
/// When encoded, an ack header has the following format:
/// ```text
///        1 byte         1 byte     2 bytes
///        -------------------------------------------------
/// ACK   | Block # MSB | 04     |   Block # lower 2 bytes  |
///        -------------------------------------------------
/// ```
#[derive(Clone, Debug)]
pub struct AckHeader { pub block_number: u32 }

impl AckHeader {
    pub fn new(block_number: u32) -> Self { AckHeader { block_number } }
    pub fn into_raw(self) -> RawRequest { self.into() }
    pub fn from_raw(src: RawResponse) -> TFTPResult<AckHeader> {
        debug_assert!(src[1] == OPCODE_ACK);
        // There is no reason an Ack should have the MSB of the opcode be anything but zero.
        debug_assert!(src[0] == 0);

        if src.len() < 4 {
            return Err(TFTPError::InvalidHeaderLen)
        }
        let mut block_number = 0u32;
        block_number |= (src[0] as u32) << 16;
        block_number |= (src[2] as u32) << 8;
        block_number |= (src[3] as u32);

        Ok(AckHeader { block_number })
    }
}

impl Into<RawRequest> for AckHeader {
    fn into(self) -> RawRequest {
        let mut data = vec![0u8; 4];
        data[1] = OPCODE_ACK;

        data[0] = (self.block_number >> 16) as u8;
        data[2] = (self.block_number >> 8) as u8;
        data[3] = self.block_number as u8;

        data
    }
}

/// Represents all possible error codes defined by RFC1350. Any error code that is greater than 7
/// will be mapped to ErrorCode::Undefined.
#[repr(u16)]
#[derive(Clone, Copy, Debug)]
pub enum ErrorCode {
    Undefined = 0,
    FileNotFound = 1,
    AccessViolation = 2,
    DiskFull = 3,
    IllegalOperation = 4,
    UnknownTransferID = 5,
    FileAlreadyExists = 6,
    NoSuchUser = 7
}


impl From<u16> for ErrorCode {
    fn from(src: u16) -> Self {
        if src < 8 {
            unsafe { mem::transmute::<u16, ErrorCode>(src) }
        } else {
            ErrorCode::Undefined
        }
    }
}


/// Represents a TFTP error header.
/// The header, when encoded, has the following format:
/// ```text
///         2 bytes  2 bytes       string    1 byte
///        ----------------------------------------
/// ERROR | 05    |  ErrorCode |   ErrMsg   |   0  |
///        ----------------------------------------
/// ```
#[derive(Clone, Debug)]
pub struct ErrorHeader {

    /// Gives a hint as to what may have went wrong.
    error_code: ErrorCode,

    /// The error message of this error header. Should not contain any null (0) bytes.
    error_message: String,
}

impl ErrorHeader {
    pub fn new<T: Into<ErrorCode>>(error_code: T, error_message: String) -> Result<ErrorHeader, TFTPError> {
        if error_message.contains('\0') {
            Err(TFTPError::InvalidString)
        } else {
            Ok(ErrorHeader {
                error_message,
                error_code: error_code.into()
            })
        }
    }

    pub fn from_raw(src: RawResponse) -> TFTPResult<ErrorHeader> {
        if src.len() < 5 {
            return Err(TFTPError::InvalidHeaderLen)
        }

        debug_assert!(src[1] == OPCODE_ERROR);
        // No reason the MSB should be set for an error...
        debug_assert!(src[0] == 0);

        let error_code: ErrorCode = (((src[2] as u16) << 8) | (src[3] as u16)).into();

        // uncomment this if empty strings are not allowed.
        //debug_assert!(src[4] != 0);

        let mut error_message = Vec::with_capacity(src.len() - 5);
        let mut i = 0;
        while src[4 + i] != 0 {
            error_message.push(src[4 + i]);
            i += 1;
        }
        match String::from_utf8(error_message) {
            Ok(error_message)   => Ok(ErrorHeader { error_code, error_message }),
            Err(e)              => Err(TFTPError::InvalidUnicodeString(e))
        }
    }

    pub fn into_raw(self) -> RawRequest { self.into() }
}

impl Into<RawRequest> for ErrorHeader {
    fn into(self) -> RawRequest {
        let data_len = self.error_message.len() + 5;
        let mut data: Vec<u8> = vec![0u8; data_len];
        data[1] = OPCODE_ERROR;
        data[2] = (self.error_code as u16 >> 8) as u8;
        data[3] = (self.error_code as u16 & 0xFF) as u8;

        let error_message_bytes: &[u8] = self.error_message.as_ref();

        data[4..data_len - 1].clone_from_slice(error_message_bytes);
        data[data_len - 1] = 0;
        data
    }
}