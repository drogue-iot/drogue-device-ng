#![no_std]
#![no_main]
#![macro_use]
#![allow(incomplete_features)]
#![feature(generic_associated_types)]
#![feature(min_type_alias_impl_trait)]
#![feature(impl_trait_in_bindings)]
#![feature(type_alias_impl_trait)]
#![feature(concat_idents)]

use defmt_rtt as _;
use drogue_device::{
    actors::led::*,
    rp::{
        gpio::{Level, Output},
        peripherals::PIN_25,
        Peripherals,
    },
    time::{Duration, Timer},
    *,
};

use panic_probe as _;

#[derive(Device)]
pub struct MyDevice {
    led: ActorContext<'static, Led<Output<'static, PIN_25>>>,
}

#[drogue::main]
async fn main(context: DeviceContext<MyDevice>) {
    let p = Peripherals::take().unwrap();

    let led = Output::new(p.PIN_25, Level::Low);

    context.configure(MyDevice {
        led: ActorContext::new(Led::new(led)),
    });

    let led = context.mount(|device| {
        let led = device.led.mount(());
        led
    });

    loop {
        led.notify(LedMessage::Toggle).unwrap();
        Timer::after(Duration::from_secs(1)).await;
    }
}
