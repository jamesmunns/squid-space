#![no_main]
#![no_std]

use node_bootloader::{self as _, GlobalRollingTimer}; // global logger + panicking-behavior + memory layout

use groundhog::RollingTimer;
use hal::{
    gpio::GpioExt,
    rcc::{Config, PllConfig, Prescaler, RccExt},
    stm32,
    prelude::{OutputPin, ToggleableOutputPin}, serial::BasicConfig, time::U32Ext,
};
use squid_boot::{machine::{Flash, Machine}, icd::Parameters};
use stm32g0xx_hal as hal;
use hal::hal::serial::{Read, Write};
use hal::block;

const PARAMS: Parameters = Parameters {
    settings_max: (2 * 1024) - 4,
    data_chunk_size: 2 * 1024,
    valid_flash_range: (0x0000_0000, 0x0000_0000 + (64 * 1024)),
    valid_app_range: (0x0000_0000 + (16 * 1024), 0x0000_0000 + (64 * 1024)),
    read_max: 2 * 1024,
};

#[cortex_m_rt::entry]
fn main() -> ! {
    defmt::println!("Hello, world!");

    if let Some(_) = imain() {
        // defmt::println!("OK");
    } else {
        // defmt::println!("ERR");
    }

    node_bootloader::exit()
}

struct DefmtFlash {

}

impl Flash for DefmtFlash {
    fn flash_range(&mut self, start: u32, data: &[u8]) {
        defmt::println!(
            "FLASH RANGE => {{ start: {=u32:08X}, len: {=u32:08X} }}",
            start,
            data.len() as u32
        );
    }

    fn erase_range(&mut self, start: u32, len: u32) {
        defmt::println!(
            "ERASE RANGE => {{ start: {=u32:08X}, len: {=u32:08X} }}",
            start,
            len
        );
    }

    fn write_settings(&mut self, data: &[u8], crc: u32) {
        defmt::println!(
            "WRITE SETTINGS => {{ crc32: {=u32:08X}, len: {=u32:08X} }}",
            crc,
            data.len() as u32
        );
    }

    fn read_range(&mut self, start_addr: u32, len: u32) -> &[u8] {
        defmt::println!(
            "READ RANGE => {{ start: {=u32:08X}, len: {=u32:08X} }}",
            start_addr,
            len
        );
        &[0u8; 2048]
    }

    fn parameters(&self) -> &squid_boot::icd::Parameters {
        &PARAMS
    }

    fn boot(&mut self) -> ! {
        defmt::println!("Booting!");
        let timer = GlobalRollingTimer::new();
        for i in (0..=3).rev() {
            defmt::println!("{=u8}...", i);
            let start = timer.get_ticks();
            while timer.millis_since(start) < 1000 { }
        }
        defmt::panic!("TOTALLY A BOOT")
    }
}

fn imain() -> Option<()> {
    let board = stm32::Peripherals::take()?;
    let _core = stm32::CorePeripherals::take()?;

    // Configure clocks
    let config = Config::pll()
        .pll_cfg(PllConfig::with_hsi(1, 8, 2))
        .ahb_psc(Prescaler::NotDivided)
        .apb_psc(Prescaler::NotDivided);
    let mut rcc = board.RCC.freeze(config);

    let gpioa = board.GPIOA.split(&mut rcc);
    let _gpiob = board.GPIOB.split(&mut rcc);

    let cfg = BasicConfig::default()
        .baudrate(115200u32.bps())
        .parity_none();

    let usart2 = hal::serial::Serial::usart2(
        board.USART2,
        (gpioa.pa2, gpioa.pa3),
        cfg,
        &mut rcc,
    ).ok()?;

    GlobalRollingTimer::init(board.TIM2);

    // let mut last_a_blink = start;
    // let mut last_b_blink = start;

    let mut led_a = gpioa.pa0.into_push_pull_output();
    let mut led_b = gpioa.pa1.into_push_pull_output();
    led_a.set_high().ok();
    led_b.set_low().ok();
    let (mut tx, mut rx) = usart2.split();

    let mut buf = [0u8; 3 * 1024];
    let mut machine = Machine::new(&mut buf, DefmtFlash { });

    loop {
        {
            let val = match block!(rx.read()) {
                Ok(byte) => machine.push(byte),
                Err(_) => continue,
            };

            led_a.toggle().ok();
            led_b.toggle().ok();

            if let Some(msg) = val {
                msg.iter().for_each(|b| {
                    block!(tx.write(*b)).ok();
                })
            }
        }

        machine.check_after_send();
    }
}
