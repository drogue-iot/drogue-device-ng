#![allow(incomplete_features)]
#![feature(generic_associated_types)]
#![feature(min_type_alias_impl_trait)]
#![feature(impl_trait_in_bindings)]
#![feature(type_alias_impl_trait)]
#![feature(concat_idents)]

use core::future::Future;
use core::pin::Pin;
use drogue_device_platform_std::{self as drogue, *};

pub struct MyActor {
    name: &'static str,
    counter: u32,
}

impl MyActor {
    pub fn new(name: &'static str) -> Self {
        Self { name, counter: 0 }
    }
}

impl Actor for MyActor {
    type Message = SayHello;
    type OnStartFuture<'a> = impl Future<Output = ()> + 'a;
    type OnMessageFuture<'a> = impl Future<Output = ()> + 'a;

    fn on_start(self: Pin<&'_ mut Self>) -> Self::OnStartFuture<'_> {
        async move { log::info!("[{}] started!", self.name) }
    }

    fn on_message<'m>(
        mut self: Pin<&'m mut Self>,
        message: &'m mut Self::Message,
    ) -> Self::OnMessageFuture<'m> {
        async move {
            log::info!("[{}] hello {}: {}", self.name, message.0, self.counter);
            self.counter += 1;
        }
    }
}

pub struct SayHello(&'static str);

#[derive(drogue::Device)]
pub struct MyDevice {
    a: ActorState<'static, MyActor>,
    b: ActorState<'static, MyActor>,
}

#[drogue::configure]
fn configure() -> MyDevice {
    env_logger::builder()
        .filter_level(log::LevelFilter::Info)
        .format_timestamp_nanos()
        .init();

    MyDevice {
        a: ActorState::new(MyActor::new("a")),
        b: ActorState::new(MyActor::new("b")),
    }
}

#[drogue::main]
async fn main(context: DeviceContext<MyDevice>) {
    let a_addr = context.device().a.address();
    let b_addr = context.device().b.address();
    loop {
        Timer::after(Duration::from_secs(1)).await;
        a_addr.send(&mut SayHello("World")).await;
        b_addr.send(&mut SayHello("You")).await;
    }
}
