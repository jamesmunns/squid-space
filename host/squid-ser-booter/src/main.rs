use std::{time::Duration, io::ErrorKind, thread::sleep};

use squid_boot::{icd::{Request, Response, Parameters, ResponseError, StartBootload, DataChunk, decode_in_place}, machine::Bootable};

const PARAMS: Parameters = Parameters {
    settings_max: (2 * 1024) - 4,
    data_chunk_size: 2 * 1024,
    valid_flash_range: (0x0000_0000, 0x0000_0000 + (64 * 1024)),
    valid_app_range: (0x0000_0000 + (16 * 1024), 0x0000_0000 + (64 * 1024)),
    read_max: 2 * 1024,
};

fn main() {
    let mut port = serialport::new("/dev/ttyACM0", 115_200)
        .timeout(Duration::from_millis(10))
        .open().expect("Failed to open port");

    let last = {
        let mut last = vec![22; 2040];
        let magic = 0x10101010u32.wrapping_mul(0x10101010u32);
        last.extend_from_slice(&magic.to_le_bytes());
        last.extend_from_slice(&0x4F54_CCBCu32.to_le_bytes());
        last.leak()
    };

    // The sequence of commands sent and expected responses
    let seq: &[(Request<'static>, Result<Response<'static>, ResponseError>)] = &[
        (
            Request::GetParameters,
            Ok(Response::Parameters(PARAMS)),
        ),
        (
            Request::IsBootable,
            Ok(Response::BootableStatus(Bootable::NoBadCrc))
        ),
        (
            Request::StartBootload(StartBootload {
                start_addr: 16 * 1024,
                length: 8 * 1024,
                crc32: 0x51f3_6231,
            }),
            Ok(Response::BootloadStarted),
        ),
        (
            Request::DataChunk(DataChunk {
                data_addr: 16 * 1024,
                sub_crc32: 0x5b54_dab5,
                data: &[16; 2048],
            }),
            Ok(Response::ChunkAccepted {
                data_addr: 16 * 1024,
                data_len: 2048,
                crc32: 0x5b54_dab5,
            }),
        ),
        (
            Request::DataChunk(DataChunk {
                data_addr: 18 * 1024,
                sub_crc32: 0x8c91_77aa,
                data: &[18; 2048],
            }),
            Ok(Response::ChunkAccepted {
                data_addr: 18 * 1024,
                data_len: 2048,
                crc32: 0x8c91_77aa,
            }),
        ),
        (
            Request::DataChunk(DataChunk {
                data_addr: 20 * 1024,
                sub_crc32: 0xf01e_9d3c,
                data: &[20; 2048],
            }),
            Ok(Response::ChunkAccepted {
                data_addr: 20 * 1024,
                data_len: 2048,
                crc32: 0xf01e_9d3c,
            }),
        ),
        (
            Request::DataChunk(DataChunk {
                data_addr: 22 * 1024,
                sub_crc32: 0x514d5248,
                data: last,
            }),
            Ok(Response::ChunkAccepted {
                data_addr: 22 * 1024,
                data_len: 2048,
                crc32: 0x514d5248,
            }),
        ),
        (
            Request::CompleteBootload { boot: None },
            Ok(Response::ConfirmComplete { will_boot: false, boot_status: Bootable::Yes }),
        ),
    ];

    for (req, exp_resp) in seq.iter() {
        'retry: loop {
            println!("Sending: {:?}", req);
            let to_send = req.encode_to_vec();
            port.write_all(&to_send).unwrap();
            let mut rx = Vec::new();
            'recv: loop {
                let mut buf = [0u8; 128];
                match port.read(&mut buf) {
                    Ok(0) => panic!(),
                    Ok(n) => rx.extend_from_slice(&buf[..n]),
                    Err(e) if e.kind() == ErrorKind::TimedOut => continue 'recv,
                    Err(e) => panic!()
                }

                match rx.iter().position(|b| *b == 0) {
                    Some(n) => {
                        rx.shrink_to(n + 1);
                        break 'recv;
                    },
                    None => continue 'recv,
                }
            }

            match decode_in_place::<Result<Response<'_>, ResponseError>>(&mut rx) {
                Ok(msg) => {
                    if &msg == exp_resp {
                        println!("Got expected response: {:?}", msg);
                        sleep(Duration::from_secs(3));
                        break 'retry;
                    } else {
                        println!("Unexpected response!");
                        println!("Expected: {:?}", exp_resp);
                        println!("Actual:   {:?}", msg);
                        panic!();
                    }
                },
                Err(_) => todo!(),
            }
        }
    }
}
