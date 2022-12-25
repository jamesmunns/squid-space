#![no_main]
#![no_std]

use panic_reset as _;
use stm32g0xx_hal as _;

use groundhog::RollingTimer;
// use embedded_hal::blocking::delay::{DelayUs, DelayMs};
use core::sync::atomic::{AtomicPtr, Ordering};
use stm32g0xx_hal::stm32::{tim2::RegisterBlock as Tim2Rb, RCC, TIM2};

static TIMER_PTR: AtomicPtr<Tim2Rb> = AtomicPtr::new(core::ptr::null_mut());

pub struct GlobalRollingTimer;

impl GlobalRollingTimer {
    pub const fn new() -> Self {
        Self
    }

    pub fn init(timer: TIM2) {
        let rcc = unsafe { &*RCC::ptr() };

        rcc.apbenr1.modify(|_, w| w.tim2en().set_bit());
        rcc.apbrstr1.modify(|_, w| w.tim2rst().set_bit());
        rcc.apbrstr1.modify(|_, w| w.tim2rst().clear_bit());

        // pause
        timer.cr1.modify(|_, w| w.cen().clear_bit());
        // reset counter
        timer.cnt.reset();

        // Calculate counter configuration

        timer.psc.write(|w| w.psc().bits(63));
        timer.arr.write(|w| unsafe { w.bits(0xFFFFFFFF) });
        timer.egr.write(|w| w.ug().set_bit());
        timer.cr1.modify(|_, w| w.cen().set_bit().urs().set_bit());

        // TODO: Critical section?
        let old_ptr = TIMER_PTR.load(Ordering::SeqCst);
        TIMER_PTR.store(TIM2::ptr() as *mut _, Ordering::SeqCst);

        debug_assert!(old_ptr == core::ptr::null_mut());
    }
}

impl RollingTimer for GlobalRollingTimer {
    type Tick = u32;
    const TICKS_PER_SECOND: u32 = 1_000_000;

    fn is_initialized(&self) -> bool {
        unsafe { TIMER_PTR.load(Ordering::SeqCst).as_ref() }.is_some()
    }

    fn get_ticks(&self) -> u32 {
        if let Some(t0) = unsafe { TIMER_PTR.load(Ordering::SeqCst).as_ref() } {
            t0.cnt.read().bits()
        } else {
            0
        }
    }
}
