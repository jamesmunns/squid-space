//! Interface Control Document
//!
//! This module contains the types that are sent "over the wire"
//! for the bootloader commands and responses

use crate::{machine::Bootable, CRC};

use crc::Digest;
use postcard::ser_flavors::{Cobs, Slice};
use serde::{Deserialize, Serialize};

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct DataChunk<'a> {
    pub data_addr: u32,
    pub sub_crc32: u32,
    pub data: &'a [u8],
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct StartBootload {
    pub start_addr: u32,
    pub length: u32,
    pub crc32: u32,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub enum BootCommand {
    BootIfBootable,
    ForceBoot,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub enum Request<'a> {
    Ping(u32),
    GetParameters,
    StartBootload(StartBootload),
    DataChunk(DataChunk<'a>),
    CompleteBootload { boot: Option<BootCommand> },
    GetSettings,
    WriteSettings { data: &'a [u8] },
    GetStatus,
    ReadRange { start_addr: u32, len: u32 },
    AbortBootload,
    IsBootable,
    Boot(BootCommand),
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub enum ResponseError {
    // StartBootload responses
    BadStartAddress,
    BadLength,
    BootloadInProgress,

    // DataChunk responses
    SkippedRange { expected: u32, actual: u32 },
    IncorrectLength { expected: u32, actual: u32 },
    BadSubCrc { expected: u32, actual: u32 },
    NoBootloadActive,
    TooManyChunks,

    // CompleteBootload responses
    IncompleteLoad { expected_len: u32, actual_len: u32 },
    BadFullCrc { expected: u32, actual: u32 },

    // WriteSettings
    SettingsTooLong { max: u32, actual: u32 },

    // ReadRange
    BadRangeStart,
    BadRangeEnd,
    BadRangeLength { actual: u32, max: u32 },

    LineNak(crate::machine::Error),
    Oops,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub enum Status {
    Idle,
    Started {
        start_addr: u32,
        length: u32,
        crc32: u32,
    },
    Loading {
        start_addr: u32,
        next_addr: u32,
        partial_crc32: u32,
        expected_crc32: u32,
    },
    AwaitingComplete,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Parameters {
    pub settings_max: u32,
    pub data_chunk_size: u32,
    pub valid_flash_range: (u32, u32),
    pub valid_app_range: (u32, u32),
    pub read_max: u32,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub enum Response<'a> {
    Pong(u32),
    Parameters(Parameters),
    BootloadStarted,
    ChunkAccepted {
        data_addr: u32,
        data_len: u32,
        crc32: u32,
    },
    ConfirmComplete {
        will_boot: bool,
        boot_status: Bootable,
    },
    Settings {
        data: &'a [u8],
    },
    SettingsAccepted {
        data_len: u32,
    },
    Status(Status),
    ReadRange {
        start_addr: u32,
        len: u32,
        data: &'a [u8],
    },
    BadOverfillNak,
    BadPostcardNak,
    BadCrcNak,
    BootloadAborted,
    BootableStatus(Bootable),
    ConfirmBootCmd {
        will_boot: bool,
        boot_status: Bootable,
    },
}

#[cfg(feature = "use-std")]
impl<'a> Request<'a> {
    /// Encode a request to a vec.
    ///
    /// Does:
    ///
    /// * postcard encoding
    /// * appending crc32 (le)
    /// * cobs encoding
    /// * DOES append `0x00` terminator
    pub fn encode_to_vec(&self) -> Vec<u8> {
        use postcard::ser_flavors::StdVec;
        postcard::serialize_with_flavor::<Self, Crc32SerFlavor<Cobs<StdVec>>, Vec<u8>>(
            self,
            Crc32SerFlavor {
                flav: Cobs::try_new(StdVec::new()).unwrap(),
                checksum: CRC.digest(),
            },
        )
        .unwrap()
    }
}

#[cfg(feature = "use-std")]
impl<'a> Response<'a> {
    /// Encode a request to a vec.
    ///
    /// Does:
    ///
    /// * postcard encoding
    /// * appending crc32 (le)
    /// * cobs encoding
    /// * DOES append `0x00` terminator
    pub fn encode_to_vec(&self) -> Vec<u8> {
        use postcard::ser_flavors::StdVec;
        postcard::serialize_with_flavor::<Self, Crc32SerFlavor<Cobs<StdVec>>, Vec<u8>>(
            self,
            Crc32SerFlavor {
                flav: Cobs::try_new(StdVec::new()).unwrap(),
                checksum: CRC.digest(),
            },
        )
        .unwrap()
    }
}

#[inline]
pub fn encode_resp_to_slice<'a, 'b>(
    resp: &Result<Response<'a>, ResponseError>,
    buf: &'b mut [u8],
) -> Result<&'b mut [u8], postcard::Error> {
    postcard::serialize_with_flavor::<
        Result<Response<'a>, ResponseError>,
        Crc32SerFlavor<Cobs<Slice<'b>>>,
        &'b mut [u8],
    >(
        resp,
        Crc32SerFlavor {
            flav: Cobs::try_new(Slice::new(buf))?,
            checksum: CRC.digest(),
        },
    )
}

#[inline]
pub fn decode_in_place<'a, T: Deserialize<'a>>(
    buf: &'a mut [u8],
) -> Result<T, crate::machine::Error> {
    let used = cobs::decode_in_place(buf).map_err(|_| crate::machine::Error::Cobs)?;
    let buf = buf
        .get_mut(..used)
        .ok_or(crate::machine::Error::LogicError)?;
    if used < 5 {
        return Err(crate::machine::Error::Underfill);
    }
    let (data, crc) = buf.split_at_mut(used - 4);
    let mut crc_bytes = [0u8; 4];
    crc_bytes.copy_from_slice(crc);
    let exp_crc = u32::from_le_bytes(crc_bytes);
    let act_crc = CRC.checksum(data);
    if exp_crc != act_crc {
        return Err(crate::machine::Error::Crc {
            expected: exp_crc,
            actual: act_crc,
        });
    }
    postcard::from_bytes(data).map_err(|_| crate::machine::Error::PostcardDecode)
}

struct Crc32SerFlavor<B>
where
    B: postcard::ser_flavors::Flavor,
{
    flav: B,
    checksum: Digest<'static, u32>,
}

impl<B> postcard::ser_flavors::Flavor for Crc32SerFlavor<B>
where
    B: postcard::ser_flavors::Flavor,
{
    type Output = <B as postcard::ser_flavors::Flavor>::Output;

    #[inline]
    fn try_push(&mut self, data: u8) -> postcard::Result<()> {
        self.checksum.update(&[data]);
        self.flav.try_push(data)
    }

    #[inline]
    fn finalize(mut self) -> postcard::Result<Self::Output> {
        let calc_crc = self.checksum.finalize();
        self.flav.try_extend(&calc_crc.to_le_bytes())?;
        self.flav.finalize()
    }

    #[inline]
    fn try_extend(&mut self, data: &[u8]) -> postcard::Result<()> {
        self.checksum.update(data);
        self.flav.try_extend(data)
    }
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct Setting<'a> {
    pub name_ascii: &'a [u8],
    pub val: SettingVal<'a>,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub enum SettingVal<'a> {
    U32(u32),
    F32(f32),
    ByteSlice(&'a [u8]),
    AsciiSlice(&'a [u8]),
}

pub struct SettingsIter<'a> {
    remain: &'a [u8],
}

impl<'a> Iterator for SettingsIter<'a> {
    type Item = Setting<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let rem = core::mem::take(&mut self.remain);
        match postcard::take_from_bytes(rem) {
            Ok((t, remain)) => {
                self.remain = remain;
                Some(t)
            }
            Err(_) => {
                // DON'T replace the buffer, we get one bad: we're done here.
                None
            }
        }
    }
}

#[cfg(feature = "use-std")]
pub fn settings_to_vec(items: &[Setting<'_>]) -> Vec<u8> {
    // Build the settings block. First we serialize all the fields
    let mut ser = Vec::new();
    for stg in items.iter() {
        ser.extend_from_slice(&postcard::to_stdvec(stg).unwrap());
    }

    // Now we get the serialized length, and calculate len + serialized data
    let len = ser.len() as u32;
    let mut digest = CRC.digest();
    digest.update(&len.to_le_bytes());
    digest.update(&ser);
    let crc = digest.finalize();

    // Build the "actual" serialized data, starting with the CRC, then the len,
    // then the data.
    let mut ser2 = Vec::new();
    ser2.extend_from_slice(&crc.to_le_bytes());
    ser2.extend_from_slice(&len.to_le_bytes());
    ser2.extend_from_slice(&ser);

    ser2
}

pub fn settings_from_raw(sli: &[u8]) -> Result<SettingsIter<'_>, ()> {
    let (exp_crc, sli) = split_u32le(sli)?;
    let (exp_len, sli) = split_u32le(sli)?;
    let settings_bytes = sli.get(..(exp_len as usize)).ok_or(())?;
    let mut digest = CRC.digest();
    digest.update(&exp_len.to_le_bytes());
    digest.update(settings_bytes);
    let act_crc = digest.finalize();

    if act_crc == exp_crc {
        Ok(SettingsIter {
            remain: settings_bytes,
        })
    } else {
        Err(())
    }
}

#[inline]
pub fn split_u32le(sli: &[u8]) -> Result<(u32, &[u8]), ()> {
    if sli.len() < 4 {
        return Err(());
    }
    let (bytes, remain) = sli.split_at(4);
    let mut buf = [0u8; 4];
    buf.copy_from_slice(bytes);
    Ok((u32::from_le_bytes(buf), remain))
}

#[cfg(test)]
pub mod test {
    use crate::icd::{settings_from_raw, settings_to_vec, Setting, SettingVal};

    #[test]
    fn settings_smoke() {
        let stgs = &[
            Setting {
                name_ascii: b"my",
                val: SettingVal::ByteSlice(b"MY"),
            },
            Setting {
                name_ascii: b"eyes",
                val: SettingVal::AsciiSlice(b"BRAND"),
            },
            Setting {
                name_ascii: b"are",
                val: SettingVal::U32(0x1234_5678),
            },
            Setting {
                name_ascii: b"special",
                val: SettingVal::F32(core::f32::consts::PI),
            },
        ];

        let stgs_vec = settings_to_vec(stgs);

        // Now read it back, and collect it so we can check we got the right
        // number of items
        let stg_iter = settings_from_raw(&stgs_vec).unwrap();
        let des_stgs = stg_iter.collect::<Vec<_>>();
        assert_eq!(stgs.len(), des_stgs.len());

        // Make sure the items match 1:1 (and the order is the same, but this
        // is more of an impl detail).
        des_stgs.iter().zip(stgs).for_each(|(des, exp)| {
            assert_eq!(des, exp);
        });
    }
}
