#![no_std]
#![no_main]
#![macro_use]
#![allow(incomplete_features)]
#![feature(generic_associated_types)]
#![feature(min_type_alias_impl_trait)]
#![feature(impl_trait_in_bindings)]
#![feature(type_alias_impl_trait)]
#![feature(concat_idents)]

use log::LevelFilter;
use panic_probe as _;
use rtt_logger::RTTLogger;
use rtt_target::rtt_init_print;

use core::{cell::UnsafeCell, future::Future, pin::Pin, str::FromStr};
use drogue_device::{
    actors::button::Button,
    drivers::network::esp8266::*,
    nrf::{
        buffered_uarte::BufferedUarte,
        gpio::{AnyPin, Input, Level, NoPin, Output, OutputDrive, Pull},
        gpiote::{self, PortInput},
        interrupt,
        peripherals::{P0_02, P0_03, P0_14, TIMER0, UARTE0},
        uarte, Peripherals,
    },
    time::Duration,
    traits::network::{ip::*, tcp::*, wifi::*},
    *,
};
use heapless::{consts, Vec};

const WIFI_SSID: &str = include_str!(concat!(env!("OUT_DIR"), "/config/wifi.ssid.txt"));
const WIFI_PSK: &str = include_str!(concat!(env!("OUT_DIR"), "/config/wifi.password.txt"));

static LOGGER: RTTLogger = RTTLogger::new(LevelFilter::Info);

#[derive(Device)]
pub struct MyDevice {
    driver: UnsafeCell<Esp8266Driver>,
    modem: ActorContext<'static, Esp8266ModemActor>,
    //    button: ActorContext<'static, Button<'static, PortInput<'static, P0_14>, Statistics>>,
}

type UART = BufferedUarte<'static, UARTE0, TIMER0>;
type ENABLE = Output<'static, P0_03>;
type RESET = Output<'static, P0_02>;

pub struct Esp8266ModemActor {
    modem: Option<Esp8266Modem<'static, UART, ENABLE, RESET>>,
}

impl Esp8266ModemActor {
    pub fn new() -> Self {
        Self { modem: None }
    }
}

impl Unpin for Esp8266ModemActor {}

impl Actor for Esp8266ModemActor {
    type Configuration = Esp8266Modem<'static, UART, ENABLE, RESET>;
    type Message<'m> = ();

    fn on_mount(&mut self, config: Self::Configuration) {
        self.modem.replace(config);
    }

    type OnStartFuture<'m> = impl Future<Output = ()> + 'm;
    fn on_start(mut self: Pin<&'_ mut Self>) -> Self::OnStartFuture<'_> {
        async move {
            self.modem.as_mut().unwrap().run().await;
        }
    }

    type OnMessageFuture<'m> = ImmediateFuture;
    fn on_message<'m>(
        self: Pin<&'m mut Self>,
        _: &'m mut Self::Message<'m>,
    ) -> Self::OnMessageFuture<'m> {
        ImmediateFuture::new()
    }
}

#[drogue::main]
async fn main(context: DeviceContext<MyDevice>) {
    rtt_init_print!();
    unsafe {
        log::set_logger_racy(&LOGGER).unwrap();
    }

    log::set_max_level(log::LevelFilter::Info);
    let p = Peripherals::take().unwrap();

    let g = gpiote::initialize(p.GPIOTE, interrupt::take!(GPIOTE));
    let button_port = PortInput::new(g, Input::new(p.P0_14, Pull::Up));

    let mut config = uarte::Config::default();
    config.parity = uarte::Parity::EXCLUDED;
    config.baudrate = uarte::Baudrate::BAUD115200;

    static mut TX_BUFFER: [u8; 256] = [0u8; 256];
    static mut RX_BUFFER: [u8; 256] = [0u8; 256];

    let irq = interrupt::take!(UARTE0_UART0);
    let u = unsafe {
        BufferedUarte::new(
            p.UARTE0,
            p.TIMER0,
            p.PPI_CH0,
            p.PPI_CH1,
            irq,
            p.P0_13,
            p.P0_01,
            NoPin,
            NoPin,
            config,
            &mut RX_BUFFER,
            &mut TX_BUFFER,
        )
    };

    let enable_pin = Output::new(p.P0_03, Level::Low, OutputDrive::Standard);
    let reset_pin = Output::new(p.P0_02, Level::Low, OutputDrive::Standard);

    context.configure(MyDevice {
        driver: UnsafeCell::new(Esp8266Driver::new()),
        modem: ActorContext::new(Esp8266ModemActor::new()),
        //button: ActorContext::new(Button::new(button_port)),
    });

    log::info!("Initializing");

    let mut controller = context.mount(|device| {
        let (controller, modem) =
            unsafe { &mut *device.driver.get() }.initialize(u, enable_pin, reset_pin);
        device.modem.mount(modem);
        controller
    });

    controller
        .join(Join::Wpa {
            ssid: heapless::String::from_str(WIFI_SSID).unwrap(),
            password: heapless::String::from_str(WIFI_PSK).unwrap(),
        })
        .await
        .expect("Error joining wifi");

    log::info!("Wifi Connected!");

    let mut socket = controller.open().await;

    log::info!("Socket opened");

    let ip: IpAddress = IpAddress::new_v4(192, 168, 1, 2);
    let port: u16 = 12345;

    let result = controller
        .connect(socket, IpProtocol::Tcp, SocketAddress::new(ip, port))
        .await;
    match result {
        Ok(_) => {
            log::info!("Connected!");
        }
        Err(e) => {
            log::info!("Error connecting to host: {:?}", e);
        }
    }
}
