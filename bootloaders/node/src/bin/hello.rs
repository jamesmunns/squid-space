#![no_main]
#![no_std]

use node_bootloader as _;

#[cortex_m_rt::entry]
fn main() -> ! {
    loop {
        cortex_m::asm::delay(64_000_000);
    }
}
