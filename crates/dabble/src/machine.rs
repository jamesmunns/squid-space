use core::mem::replace;

use crate::{
    icd::{
        settings_from_raw, BootCommand, DataChunk, Parameters, Request, Response, ResponseError,
        Setting, SettingVal, StartBootload, Status,
    },
    CRC,
};
use crc::Digest;
use serde::{Deserialize, Serialize};

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub enum Error {
    Underfill,
    Overfill,
    PostcardDecode,
    Cobs,
    Crc { expected: u32, actual: u32 },
    LogicError,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub enum Bootable {
    Unsure,
    NoMissingSettings,
    NoDuplicateSettings,
    NoInvalidSettings,
    NoInvalidCrc,
    Yes { crc32: u32, length: usize },
}

pub trait Flash {
    const PARAMETERS: Parameters;

    /// Program the following block of data to the address starting at start
    fn flash_range(&mut self, start: u32, data: &[u8]);

    /// Erase the block of data in the given range
    fn erase_range(&mut self, start: u32, len: u32);

    /// Read the entire raw settings page, including length and crc data
    fn read_settings_raw(&mut self) -> &[u8];

    /// Write the given raw settings to the settings page. If the settings
    /// page requires erase, the trait IMPLEMENTOR must do this in the
    /// implementation of `write_settings`.
    fn write_settings(&mut self, data: &[u8]);

    /// Read a given range of flash data
    fn read_range(&mut self, start_addr: u32, len: u32) -> &[u8];

    /// Boot to the application
    fn boot(&mut self) -> !;

    /// Is the system currently capable of booting into the application?
    fn is_bootable(&mut self) -> Bootable {
        let pre_check = get_app_info(self.read_settings_raw(), &Self::PARAMETERS);
        let (app_crc, app_len) = match pre_check {
            Bootable::Yes { crc32, length } => (crc32, length as u32),
            nope => return nope,
        };

        let mut digest = CRC.digest();
        let start = Self::PARAMETERS.valid_app_range.0;
        let end = start + app_len;
        let chunk_len = Self::PARAMETERS.data_chunk_size;

        let mut cur = start;
        while cur < end {
            let cur_page = self.read_range(cur, chunk_len);
            digest.update(cur_page);
            cur = cur.saturating_add(chunk_len);
        }

        let act_crc = digest.finalize();
        if act_crc == app_crc {
            Bootable::Yes {
                crc32: act_crc,
                length: app_len as usize,
            }
        } else {
            Bootable::NoInvalidCrc
        }
    }
}

fn get_app_info(raw_stg: &[u8], params: &Parameters) -> Bootable {
    let mut app_len = None;
    let mut app_crc = None;

    let settings_iter = match settings_from_raw(raw_stg) {
        Ok(si) => si,
        Err(_) => return Bootable::NoMissingSettings,
    };
    for stg in settings_iter {
        match stg {
            Setting {
                name_ascii: b"app_len",
                val: SettingVal::U32(len),
            } => {
                if app_len.is_some() {
                    return Bootable::NoDuplicateSettings;
                }
                app_len = Some(len);
            }
            Setting {
                name_ascii: b"app_crc",
                val: SettingVal::U32(crc),
            } => {
                if app_crc.is_some() {
                    return Bootable::NoDuplicateSettings;
                }
                app_crc = Some(crc);
            }
            _ => {}
        }
    }

    let app_len = if let Some(len) = app_len {
        len
    } else {
        return Bootable::NoMissingSettings;
    };
    let app_crc = if let Some(crc) = app_crc {
        crc
    } else {
        return Bootable::NoMissingSettings;
    };

    let (start, end) = params.valid_app_range;
    let chunk_len = params.data_chunk_size;
    let ttl_len = end - start;

    // These checks are really just for whether the trait impl
    // is correct. They shouldn't be necessary at runtime
    #[cfg(debug_assertions)]
    {
        let read_too_small = params.read_max < params.data_chunk_size;
        let not_one_page = end < (start.saturating_add(chunk_len));
        let page_too_small = chunk_len < 8;
        let backwards = end <= start;
        let not_pow2 = !chunk_len.is_power_of_two();
        let fail_check = read_too_small || not_one_page || page_too_small || backwards || not_pow2;
        debug_assert!(!fail_check, "TODO: BYO is_bootable!");
    }

    let too_long = app_len > ttl_len;
    let too_short = app_len < chunk_len;
    let not_pow2 = !app_len.is_power_of_two();
    let fail_check = too_long || too_short || not_pow2;
    if fail_check {
        return Bootable::NoInvalidSettings;
    }

    Bootable::Yes {
        crc32: app_crc,
        length: app_len as usize,
    }
}

struct BootLoadMeta {
    digest_running: Digest<'static, u32>,
    addr_start: u32,
    addr_current: u32,
    length: u32,
    exp_crc: u32,
}

enum Mode {
    Idle,
    BootLoad(BootLoadMeta),
    BootPending,
}

#[allow(dead_code)]
const fn stm32g031_params() -> Parameters {
    Parameters {
        settings_max: (2 * 1024) - 4,
        data_chunk_size: 2 * 1024,
        valid_flash_range: (0, 64 * 1024),
        valid_app_range: (16 * 1024, 64 * 1024),
        read_max: 2 * 1024,
    }
}

pub struct Machine<HW: Flash> {
    mode: Mode,
    hardware: HW,
}

impl<HW: Flash> Machine<HW> {
    pub fn new(hw: HW) -> Self {
        Self {
            mode: Mode::Idle,
            hardware: hw,
        }
    }

    /// This function should be called after sending.
    ///
    /// At the moment, all this does is reboot the device
    /// if a boot was requested
    pub fn check_after_send(&mut self) {
        if matches!(self.mode, Mode::BootPending) {
            self.hardware.boot();
        }
    }

    /// Process incoming messages, optionally preparing a response.
    ///
    /// Most messages have a dedicated handler function, located in the impl block below
    pub fn process<'a>(&mut self, buf: &'a mut [u8]) -> Option<&'a [u8]> {
        let resp: Result<Response<'static>, ResponseError> = match crate::icd::decode_in_place::<
            Request<'_>,
        >(buf)
        {
            Ok(Request::Ping(n)) => Ok(Response::Pong(n)),
            Ok(Request::GetParameters) => Ok(Response::Parameters(HW::PARAMETERS)),
            Ok(Request::StartBootload(sb)) => self.handle_start_bootload(sb),
            Ok(Request::DataChunk(dc)) => self.handle_data_chunk(dc),
            Ok(Request::CompleteBootload { boot }) => self.handle_complete_bootload(boot),
            Ok(Request::GetSettings) => Ok(Response::Settings { data: &[] }),
            Ok(Request::WriteSettings { data }) => self.handle_write_settings(data),
            Ok(Request::GetStatus) => self.handle_get_status(),
            Ok(Request::ReadRange { start_addr, len }) => self.handle_read_range(start_addr, len),
            Ok(Request::AbortBootload) => self.handle_abort_bootload(),
            Ok(Request::IsBootable) => Ok(Response::BootableStatus(self.hardware.is_bootable())),
            Ok(Request::Boot(cmd)) => self.handle_boot(cmd),
            Err(e) => Err(ResponseError::LineNak(e)),
        };
        self.respond(resp, buf)
    }

    #[inline]
    fn respond<'a>(
        &mut self,
        msg: Result<Response<'static>, ResponseError>,
        buf: &'a mut [u8],
    ) -> Option<&'a [u8]> {
        let msg = match msg {
            Ok(ok_msg) => Ok(match ok_msg {
                // These require "re-work"! This is because we need to
                // "let go" of the borrow of buffer before we can serialize
                // other borrowed data into the buffer.
                //
                // lifetimes are hard
                Response::ReadRange {
                    start_addr,
                    len,
                    data: _,
                } => {
                    let read = self.hardware.read_range(start_addr, len);
                    Response::ReadRange {
                        start_addr,
                        len,
                        data: read,
                    }
                }
                Response::Settings { .. } => Response::Settings {
                    data: self.hardware.read_settings_raw(),
                },
                other => other,
            }),
            Err(err_msg) => Err(err_msg),
        };

        crate::icd::encode_resp_to_slice(&msg, buf)
            .ok()
            .map(|b| &*b)
    }
}

/// State Machine Handler Methods
///
/// These are dispatched by `Machine::process`.
impl<HW: Flash> Machine<HW> {
    /// Handles `Request::StartBootload`
    fn handle_start_bootload(
        &mut self,
        sb: StartBootload,
    ) -> Result<Response<'static>, ResponseError> {
        let response;
        self.mode = match replace(&mut self.mode, Mode::Idle) {
            Mode::Idle => {
                let (resp, mode) = self.start_inner(sb);
                response = resp;
                mode
            }
            Mode::BootLoad(meta) => {
                response = Err(ResponseError::BootloadInProgress);
                Mode::BootLoad(meta)
            }
            Mode::BootPending => {
                response = Err(ResponseError::Oops);
                Mode::BootPending
            }
        };
        response
    }

    fn start_inner(
        &mut self,
        sb: StartBootload,
    ) -> (Result<Response<'static>, ResponseError>, Mode) {
        if sb.start_addr != HW::PARAMETERS.valid_app_range.0 {
            return (Err(ResponseError::BadStartAddress), Mode::Idle);
        }
        let max_app_len = HW::PARAMETERS.valid_app_range.1 - HW::PARAMETERS.valid_app_range.0;
        let too_long = sb.length > max_app_len;
        let mask = HW::PARAMETERS.data_chunk_size - 1;
        let not_full = (sb.length & mask) != 0;
        if too_long || not_full {
            return (Err(ResponseError::BadLength), Mode::Idle);
        }

        self.hardware.erase_range(sb.start_addr, sb.length);

        (
            Ok(Response::BootloadStarted),
            Mode::BootLoad(BootLoadMeta {
                digest_running: CRC.digest(),
                addr_start: sb.start_addr,
                addr_current: sb.start_addr,
                length: sb.length,
                exp_crc: sb.crc32,
            }),
        )
    }

    /// Handles `Request::DataChunk`
    fn handle_data_chunk(&mut self, dc: DataChunk) -> Result<Response<'static>, ResponseError> {
        let response;
        self.mode = match replace(&mut self.mode, Mode::Idle) {
            Mode::Idle => {
                response = Err(ResponseError::NoBootloadActive);
                Mode::Idle
            }
            Mode::BootLoad(meta) => {
                let (resp, mode) = self.data_chunk_inner(meta, dc);
                response = resp;
                mode
            }
            Mode::BootPending => {
                response = Err(ResponseError::NoBootloadActive);
                Mode::BootPending
            }
        };
        response
    }

    fn data_chunk_inner(
        &mut self,
        mut meta: BootLoadMeta,
        dc: DataChunk<'_>,
    ) -> (Result<Response<'static>, ResponseError>, Mode) {
        if dc.data_addr != meta.addr_current {
            return (
                Err(ResponseError::SkippedRange {
                    expected: meta.addr_current,
                    actual: dc.data_addr,
                }),
                Mode::BootLoad(meta),
            );
        }
        if dc.data.len() as u32 != HW::PARAMETERS.data_chunk_size {
            return (
                Err(ResponseError::IncorrectLength {
                    expected: HW::PARAMETERS.data_chunk_size,
                    actual: dc.data.len() as u32,
                }),
                Mode::BootLoad(meta),
            );
        }
        if meta.addr_current >= (meta.addr_start + meta.length) {
            return (Err(ResponseError::TooManyChunks), Mode::BootLoad(meta));
        }

        let calc_crc = CRC.checksum(dc.data);
        if calc_crc != dc.sub_crc32 {
            return (
                Err(ResponseError::BadSubCrc {
                    expected: dc.sub_crc32,
                    actual: calc_crc,
                }),
                Mode::BootLoad(meta),
            );
        }

        self.hardware.flash_range(dc.data_addr, dc.data);
        meta.digest_running.update(dc.data);
        meta.addr_current += HW::PARAMETERS.data_chunk_size;

        (
            Ok(Response::ChunkAccepted {
                data_addr: dc.data_addr,
                data_len: dc.data.len() as u32,
                crc32: calc_crc,
            }),
            Mode::BootLoad(meta),
        )
    }

    /// Handles `Request::CompleteBootload`
    fn handle_complete_bootload(
        &mut self,
        boot: Option<BootCommand>,
    ) -> Result<Response<'static>, ResponseError> {
        let response;
        self.mode = match replace(&mut self.mode, Mode::Idle) {
            Mode::Idle => {
                response = Err(ResponseError::NoBootloadActive);
                Mode::Idle
            }
            Mode::BootLoad(meta) => {
                let (resp, mode) = self.complete_inner(meta, boot);
                response = resp;
                mode
            }
            Mode::BootPending => {
                response = Err(ResponseError::NoBootloadActive);
                Mode::BootPending
            }
        };
        response
    }

    fn complete_inner(
        &mut self,
        meta: BootLoadMeta,
        boot_cmd: Option<BootCommand>,
    ) -> (Result<Response<'static>, ResponseError>, Mode) {
        let complete = meta.addr_current == (meta.addr_start + meta.length);
        let response;
        let mode = if !complete {
            response = Err(ResponseError::IncompleteLoad {
                expected_len: meta.length,
                actual_len: meta.addr_current - meta.addr_start,
            });
            Mode::BootLoad(meta)
        } else {
            let calc_crc = meta.digest_running.finalize();
            if calc_crc != meta.exp_crc {
                response = Err(ResponseError::BadFullCrc {
                    expected: meta.exp_crc,
                    actual: calc_crc,
                });
                Mode::Idle
            } else {
                let boot_status = self.hardware.is_bootable();

                let will_boot = match boot_cmd {
                    Some(BootCommand::ForceBoot) => true,
                    Some(BootCommand::BootIfBootable) => {
                        matches!(boot_status, Bootable::Yes { .. })
                    }
                    None => false,
                };

                response = Ok(Response::ConfirmComplete {
                    will_boot,
                    boot_status,
                });

                if will_boot {
                    Mode::BootPending
                } else {
                    Mode::Idle
                }
            }
        };
        (response, mode)
    }

    /// Handles `Request::WriteSettings`
    fn handle_write_settings(&mut self, data: &[u8]) -> Result<Response<'static>, ResponseError> {
        if data.len() as u32 > HW::PARAMETERS.settings_max {
            return Err(ResponseError::SettingsTooLong {
                max: HW::PARAMETERS.settings_max,
                actual: data.len() as u32,
            });
        }
        self.hardware.write_settings(data);
        Ok(Response::SettingsAccepted {
            data_len: data.len() as u32,
        })
    }

    /// Handles `Request::GetStatus`
    fn handle_get_status(&mut self) -> Result<Response<'static>, ResponseError> {
        Ok(Response::Status({
            match &self.mode {
                Mode::Idle => Status::Idle,
                Mode::BootPending => Status::Idle,
                Mode::BootLoad(meta) => {
                    if meta.addr_start == meta.addr_current {
                        Status::Started {
                            start_addr: meta.addr_start,
                            length: meta.length,
                            crc32: meta.exp_crc,
                        }
                    } else if meta.addr_current == (meta.addr_start + meta.length) {
                        Status::AwaitingComplete
                    } else {
                        Status::Loading {
                            start_addr: meta.addr_start,
                            next_addr: meta.addr_current,
                            partial_crc32: meta.digest_running.clone().finalize(),
                            expected_crc32: meta.exp_crc,
                        }
                    }
                }
            }
        }))
    }

    /// Handles `Request::ReadRange`
    fn handle_read_range(
        &mut self,
        start_addr: u32,
        len: u32,
    ) -> Result<Response<'static>, ResponseError> {
        let start_ok = start_addr >= HW::PARAMETERS.valid_flash_range.0;
        if !start_ok {
            return Err(ResponseError::BadRangeStart);
        }

        match start_addr.checked_add(len) {
            Some(end) if end <= HW::PARAMETERS.valid_flash_range.1 => Ok(Response::ReadRange {
                start_addr,
                len,
                data: &[],
            }),
            _ => Err(ResponseError::BadRangeEnd),
        }
    }

    /// Handles Request::AbortBootload
    fn handle_abort_bootload(&mut self) -> Result<Response<'static>, ResponseError> {
        let mode = replace(&mut self.mode, Mode::Idle);
        let response;
        self.mode = match mode {
            Mode::Idle => {
                response = Err(ResponseError::NoBootloadActive);
                Mode::Idle
            }
            Mode::BootLoad(_meta) => {
                response = Ok(Response::BootloadAborted);
                Mode::Idle
            }
            Mode::BootPending => {
                response = Err(ResponseError::NoBootloadActive);
                Mode::BootPending
            }
        };
        response
    }

    /// Handles `Request::Boot`
    fn handle_boot(&mut self, cmd: BootCommand) -> Result<Response<'static>, ResponseError> {
        let boot_status = self.hardware.is_bootable();
        let will_boot = match cmd {
            BootCommand::BootIfBootable => matches!(boot_status, Bootable::Yes { .. }),
            BootCommand::ForceBoot => true,
        };
        self.mode = Mode::BootPending;
        Ok(Response::ConfirmBootCmd {
            will_boot,
            boot_status,
        })
    }
}

#[cfg(test)]
pub mod feat_test {
    #[test]
    fn features() {
        if !cfg!(feature = "use-std") {
            println!();
            println!("!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!");
            println!("run tests with 'use-std' feature enabled!");
            println!("!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!");
            println!();
            panic!();
        }
    }
}

#[cfg(all(test, feature = "use-std"))]
pub mod test {
    use super::Flash;
    use crate::{
        icd::{
            decode_in_place, settings_to_vec, DataChunk, Parameters, Request, Response,
            ResponseError, Setting, SettingVal, StartBootload,
        },
        machine::{stm32g031_params, Bootable, Machine, Mode},
        CRC,
    };
    use std::sync::{Arc, Mutex};

    struct HwInner {
        flash: Vec<u8>,
        settings: Vec<u8>,
    }

    #[derive(Clone)]
    struct AtomicHardware {
        inner: Arc<Mutex<HwInner>>,
    }

    impl AtomicHardware {
        pub fn new() -> Self {
            let params = Self::PARAMETERS;
            assert_eq!(params.valid_flash_range.0, 0);
            Self {
                inner: Arc::new(Mutex::new(HwInner {
                    flash: vec![0xA5u8; params.valid_flash_range.1 as usize],
                    settings: vec![0xCCu8; 4usize + params.settings_max as usize],
                })),
            }
        }
    }

    impl Flash for AtomicHardware {
        const PARAMETERS: Parameters = stm32g031_params();

        fn flash_range(&mut self, start: u32, data: &[u8]) {
            assert_eq!(Self::PARAMETERS.valid_flash_range.0, 0);
            let mut inner = self.inner.lock().unwrap();
            let su = start as usize;
            let range = inner.flash.get_mut(su..su + data.len()).unwrap();

            range
                .iter()
                .for_each(|b| assert_eq!(*b, 0xFF, "flash not erased!"));
            range.copy_from_slice(data)
        }

        fn erase_range(&mut self, start: u32, len: u32) {
            assert_eq!(Self::PARAMETERS.valid_flash_range.0, 0);
            let mut inner = self.inner.lock().unwrap();
            let su = start as usize;
            let lu = len as usize;
            inner
                .flash
                .get_mut(su..su + lu)
                .unwrap()
                .iter_mut()
                .for_each(|b| *b = 0xFF);
        }

        fn write_settings(&mut self, data: &[u8]) {
            let mut inner = self.inner.lock().unwrap();
            inner
                .settings
                .get_mut(..data.len())
                .unwrap()
                .copy_from_slice(data);
        }

        fn read_range(&mut self, start: u32, len: u32) -> &[u8] {
            assert_eq!(Self::PARAMETERS.valid_flash_range.0, 0);
            let inner = self.inner.lock().unwrap();
            let su = start as usize;
            let lu = len as usize;
            let vec = inner.flash.get(su..su + lu).unwrap().to_vec();

            // This is: uh, not great.
            vec.leak()
        }

        fn boot(&mut self) -> ! {
            todo!()
        }

        fn read_settings_raw(&mut self) -> &[u8] {
            let inner = self.inner.lock().unwrap();
            // This is: uh, not great.
            inner.settings.clone().leak()
        }
    }

    #[test]
    fn do_a_bootload() {
        // Create a fake (in-memory) hardware impl
        let hw = AtomicHardware::new();

        // Create the bootload "server": this usually runs on-device
        let mut machine = Machine::new(hw.clone());

        let mut digest = CRC.digest();
        digest.update(&[16; 2048]);
        digest.update(&[18; 2048]);
        digest.update(&[20; 2048]);
        digest.update(&[22; 2048]);
        let ttl_crc = digest.finalize();

        let settings = settings_to_vec(&[
            Setting {
                name_ascii: b"app_len",
                val: SettingVal::U32(8 * 1024),
            },
            Setting {
                name_ascii: b"app_crc",
                val: SettingVal::U32(ttl_crc),
            },
        ]);

        // The sequence of commands sent and expected responses
        let seq: &[(Request<'_>, Result<Response<'_>, ResponseError>)] = &[
            (
                Request::GetParameters,
                Ok(Response::Parameters(stm32g031_params())),
            ),
            (
                Request::IsBootable,
                Ok(Response::BootableStatus(Bootable::NoMissingSettings)),
            ),
            (
                Request::StartBootload(StartBootload {
                    start_addr: 16 * 1024,
                    length: 8 * 1024,
                    crc32: ttl_crc,
                }),
                Ok(Response::BootloadStarted),
            ),
            (
                Request::DataChunk(DataChunk {
                    data_addr: 16 * 1024,
                    sub_crc32: CRC.checksum(&[16; 2048]),
                    data: &[16; 2048],
                }),
                Ok(Response::ChunkAccepted {
                    data_addr: 16 * 1024,
                    data_len: 2048,
                    crc32: CRC.checksum(&[16; 2048]),
                }),
            ),
            (
                Request::DataChunk(DataChunk {
                    data_addr: 18 * 1024,
                    sub_crc32: CRC.checksum(&[18; 2048]),
                    data: &[18; 2048],
                }),
                Ok(Response::ChunkAccepted {
                    data_addr: 18 * 1024,
                    data_len: 2048,
                    crc32: CRC.checksum(&[18; 2048]),
                }),
            ),
            (
                Request::DataChunk(DataChunk {
                    data_addr: 20 * 1024,
                    sub_crc32: CRC.checksum(&[20; 2048]),
                    data: &[20; 2048],
                }),
                Ok(Response::ChunkAccepted {
                    data_addr: 20 * 1024,
                    data_len: 2048,
                    crc32: CRC.checksum(&[20; 2048]),
                }),
            ),
            (
                Request::DataChunk(DataChunk {
                    data_addr: 22 * 1024,
                    sub_crc32: CRC.checksum(&[22; 2048]),
                    data: &[22; 2048],
                }),
                Ok(Response::ChunkAccepted {
                    data_addr: 22 * 1024,
                    data_len: 2048,
                    crc32: CRC.checksum(&[22; 2048]),
                }),
            ),
            (
                Request::WriteSettings { data: &settings },
                Ok(Response::SettingsAccepted {
                    data_len: settings.len() as u32,
                }),
            ),
            (
                Request::CompleteBootload { boot: None },
                Ok(Response::ConfirmComplete {
                    will_boot: false,
                    boot_status: Bootable::Yes {
                        crc32: ttl_crc,
                        length: 8 * 1024,
                    },
                }),
            ),
        ];

        for (req, exp_res) in seq {
            let mut buf = [0u8; 3072];
            let enc_used = req.encode_to_vec();
            buf[..enc_used.len()].copy_from_slice(&enc_used);
            machine.process(&mut buf).unwrap();

            // Did we get a response, and is it the expected response?
            let act_res: Result<Response<'_>, ResponseError> = decode_in_place(&mut buf).unwrap();
            assert_eq!(&act_res, exp_res);
        }

        // Memory test!
        {
            let hwinner = hw.inner.lock().unwrap();
            let flash = &hwinner.flash;

            // Unprogrammed regions
            assert_eq!(&flash[..16 * 1024], [0xA5; 16 * 1024].as_slice());
            assert_eq!(&flash[24 * 1024..], [0xA5; (64 - 24) * 1024].as_slice());

            // Programmed regions
            assert_eq!(&flash[16 * 1024..][..2048], [16; 2048].as_slice());
            assert_eq!(&flash[18 * 1024..][..2048], [18; 2048].as_slice());
            assert_eq!(&flash[20 * 1024..][..2048], [20; 2048].as_slice());
            assert_eq!(&flash[22 * 1024..][..2048], [22; 2048].as_slice());
        }

        // We commanded NO reboot after flashing
        assert!(matches!(machine.mode, Mode::Idle));
    }
}
