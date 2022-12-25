#![no_main]
#![no_std]

use core::sync::atomic::Ordering;

use cortex_m::peripheral::SCB;
use node_bootloader::{self as _, GlobalRollingTimer}; // global logger + panicking-behavior + memory layout

use hal::block;
use hal::hal::serial::{Read, Write};
use hal::{
    flash::{FlashExt, FlashPage, UnlockedFlash, WriteErase},
    gpio::GpioExt,
    prelude::{OutputPin, ToggleableOutputPin},
    rcc::{Config, PllConfig, Prescaler, RccExt},
    serial::BasicConfig,
    stm32,
    time::U32Ext,
};
use squid_boot::{
    icd::Parameters,
    machine::{Flash, Machine},
};
use stm32g0xx_hal as hal;

//  0KiB - 14KiB: Bootloader
// 14KiB - 16KiB: Settings
// 16KiB - 32KiB: Application
const PARAMS: Parameters = Parameters {
    settings_max: 2 * 1024,
    data_chunk_size: 2 * 1024,
    valid_flash_range: (0, 32 * 1024),
    valid_app_range: (16 * 1024, 32 * 1024),
    read_max: 2 * 1024,
};

#[cortex_m_rt::entry]
fn main() -> ! {
    // defmt::println!("Hello, world!");

    if let Some(_) = imain() {
        // defmt::println!("OK");
    } else {
        // defmt::println!("ERR");
    }

    SCB::sys_reset();
}

struct StmFlash {
    hw: UnlockedFlash,
}

impl Flash for StmFlash {
    const PARAMETERS: Parameters = PARAMS;

    fn flash_range(&mut self, start: u32, data: &[u8]) {
        self.hw.write(start as usize, data).ok();
    }

    fn erase_range(&mut self, start: u32, len: u32) {
        let num_pages = len / 2048;
        let start = start / 2048;

        for i in start..start + num_pages {
            let page = FlashPage(i as usize);
            self.hw.erase_page(page).ok();
        }
    }

    fn read_settings_raw(&mut self) -> &[u8] {
        unsafe {
            core::sync::atomic::fence(Ordering::AcqRel);
            core::slice::from_raw_parts(0x0800_3800usize as *const u8, PARAMS.settings_max as usize)
        }
    }

    fn write_settings(&mut self, data: &[u8]) {
        self.erase_range(0x0800_3800u32, 2048);
        self.flash_range(0x0800_3800u32, data);
    }

    fn read_range(&mut self, start_addr: u32, len: u32) -> &[u8] {
        unsafe {
            core::sync::atomic::fence(Ordering::AcqRel);
            core::slice::from_raw_parts(
                (0x0800_0000usize + start_addr as usize) as *const u8,
                len as usize,
            )
        }
    }

    fn boot(&mut self) -> ! {
        // o7
        unsafe {
            let scb = &*SCB::PTR;
            scb.vtor.write(0x0800_4000);
            cortex_m::asm::bootload(0x0800_4000usize as *const u32)
        }
    }
}

fn imain() -> Option<()> {
    let board = stm32::Peripherals::take()?;
    let _core = stm32::CorePeripherals::take()?;
    let buf = cortex_m::singleton!(: [u8; 3072] = [0u8; 3072])?;

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

    let usart2 =
        hal::serial::Serial::usart2(board.USART2, (gpioa.pa2, gpioa.pa3), cfg, &mut rcc).ok()?;

    GlobalRollingTimer::init(board.TIM2);

    // let mut last_a_blink = start;
    // let mut last_b_blink = start;

    let mut led_a = gpioa.pa0.into_push_pull_output();
    let mut led_b = gpioa.pa1.into_push_pull_output();
    led_a.set_high().ok();
    led_b.set_low().ok();
    let (mut tx, mut rx) = usart2.split();

    let flash = if let Ok(ulf) = board.FLASH.unlock() {
        ulf
    } else {
        // Delay a little time so we don't reboot TOO fast
        cortex_m::asm::delay(8_000_000);
        SCB::sys_reset();
    };

    let mut machine = Machine::new(StmFlash { hw: flash });

    'process: loop {
        let mut idx = 0;
        'byte: loop {
            let cur = match buf.get_mut(idx) {
                Some(c) => c,
                None => {
                    continue 'process;
                }
            };

            match block!(rx.read()) {
                Ok(byte) => {
                    *cur = byte;
                    idx += 1;
                    if byte == 0 {
                        break 'byte;
                    }
                }
                Err(_) => continue 'byte,
            };
        }
        let val = machine.process(buf);

        led_a.toggle().ok();
        led_b.toggle().ok();

        if let Some(msg) = val {
            msg.iter().for_each(|b| {
                block!(tx.write(*b)).ok();
            })
        }

        machine.check_after_send();
    }
}
