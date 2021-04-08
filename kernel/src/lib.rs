#![allow(incomplete_features)]
#![feature(min_type_alias_impl_trait)]
#![feature(impl_trait_in_bindings)]
#![feature(generic_associated_types)]
#![feature(type_alias_impl_trait)]

pub use channel::{consts, Channel};
pub use device::{Actor, ActorState, Address, Device, DeviceContext};
pub use drogue_device_macros::{main, Device};

mod device {
    use crate::channel::{consts, Channel};
    use core::cell::{RefCell, UnsafeCell};
    use core::future::Future;
    use core::pin::Pin;
    use embassy::executor::{SpawnToken, Spawner};

    pub trait Device {
        fn mount(&'static self, spawner: Spawner);
    }

    pub struct DeviceContext<D: Device + 'static> {
        device: &'static D,
        spawner: RefCell<Option<Spawner>>,
    }

    impl<D: Device + 'static> DeviceContext<D> {
        pub fn new(device: &'static D) -> Self {
            Self {
                device,
                spawner: RefCell::new(None),
            }
        }

        pub fn device(&self) -> &'static D {
            self.device
        }

        pub fn set_spawner(&self, spawner: Spawner) {
            self.spawner.borrow_mut().replace(spawner);
        }

        pub fn start<F>(&self, token: SpawnToken<F>) {
            self.spawner
                .borrow_mut()
                .as_ref()
                .unwrap()
                .spawn(token)
                .unwrap();
        }
    }

    pub struct ActorState<'a, A: Actor> {
        pub actor: UnsafeCell<A>,
        pub channel: Channel<'a, A::Message, consts::U4>,
    }

    impl<'a, A: Actor> ActorState<'a, A> {
        pub fn new(actor: A) -> Self {
            let channel: Channel<'a, A::Message, consts::U4> = Channel::new();
            Self {
                actor: UnsafeCell::new(actor),
                channel,
            }
        }

        pub fn mount(&'a self) -> Address<'a, A> {
            self.channel.initialize();
            Address::new(&self.channel)
        }

        pub fn address(&'a self) -> Address<'a, A> {
            Address::new(&self.channel)
        }
    }

    pub trait Actor {
        type Message;
        type ProcessFuture<'a>: Future<Output = ()>
        where
            Self: 'a;

        fn process<'a>(self: Pin<&'a mut Self>, message: Self::Message) -> Self::ProcessFuture<'a>;
    }

    pub struct Address<'a, A: Actor> {
        channel: &'a Channel<'a, A::Message, consts::U4>,
    }

    impl<'a, A: Actor> Address<'a, A> {
        pub fn new(channel: &'a Channel<'a, A::Message, consts::U4>) -> Self {
            Self { channel }
        }
    }

    impl<'a, A: Actor> Address<'a, A> {
        pub async fn send(&self, message: A::Message) {
            self.channel.send(message).await
        }
    }

    impl<'a, A: Actor> Copy for Address<'a, A> {}

    impl<'a, A: Actor> Clone for Address<'a, A> {
        fn clone(&self) -> Self {
            Self {
                channel: self.channel,
            }
        }
    }
}

mod channel {

    use core::{
        cell::{RefCell, UnsafeCell},
        future::Future,
        pin::Pin,
        task::{Context, Poll},
    };
    use embassy::util::AtomicWaker;
    pub use heapless::consts;
    use heapless::{
        spsc::{Consumer, Producer, Queue},
        ArrayLength,
    };

    struct ChannelInner<'a, T, N: ArrayLength<T>> {
        queue: UnsafeCell<Queue<T, N>>,
        producer: RefCell<Option<Producer<'a, T, N>>>,
        consumer: RefCell<Option<Consumer<'a, T, N>>>,
        producer_waker: AtomicWaker,
        consumer_waker: AtomicWaker,
    }

    impl<'a, T, N: 'a + ArrayLength<T>> Default for ChannelInner<'a, T, N> {
        fn default() -> Self {
            Self::new()
        }
    }

    impl<'a, T, N: 'a + ArrayLength<T>> ChannelInner<'a, T, N> {
        pub fn new() -> Self {
            Self {
                queue: UnsafeCell::new(Queue::new()),
                producer: RefCell::new(None),
                consumer: RefCell::new(None),
                producer_waker: AtomicWaker::new(),
                consumer_waker: AtomicWaker::new(),
            }
        }

        fn split(&'a self) {
            let (producer, consumer) = unsafe { (&mut *self.queue.get()).split() };
            self.producer.borrow_mut().replace(producer);
            self.consumer.borrow_mut().replace(consumer);
        }

        fn poll_dequeue(&self, cx: &mut Context<'_>) -> Poll<T> {
            if let Some(value) = self.consumer.borrow_mut().as_mut().unwrap().dequeue() {
                self.producer_waker.wake();
                Poll::Ready(value)
            } else {
                self.consumer_waker.register(cx.waker());
                Poll::Pending
            }
        }

        fn poll_enqueue(&self, cx: &mut Context<'_>, element: &mut Option<T>) -> Poll<()> {
            let mut producer = self.producer.borrow_mut();
            if producer.as_mut().unwrap().ready() {
                let value = element.take().unwrap();
                producer.as_mut().unwrap().enqueue(value).ok().unwrap();
                self.consumer_waker.wake();
                Poll::Ready(())
            } else {
                self.producer_waker.register(cx.waker());
                Poll::Pending
            }
        }
    }

    pub struct Channel<'a, T, N: ArrayLength<T>> {
        inner: ChannelInner<'a, T, N>,
    }

    impl<'a, T, N: 'a + ArrayLength<T>> Default for Channel<'a, T, N> {
        fn default() -> Self {
            Self::new()
        }
    }

    impl<'a, T, N: 'a + ArrayLength<T>> Channel<'a, T, N> {
        pub fn new() -> Self {
            let inner = ChannelInner::new();
            Self { inner }
        }

        pub fn initialize(&'a self) {
            self.inner.split();
        }

        pub fn send(&'a self, value: T) -> ChannelSend<'a, T, N> {
            ChannelSend {
                inner: &self.inner,
                element: Some(value),
            }
        }
        pub fn receive(&'a self) -> ChannelReceive<'a, T, N> {
            ChannelReceive { inner: &self.inner }
        }
    }

    pub struct ChannelSend<'a, T, N: ArrayLength<T>> {
        inner: &'a ChannelInner<'a, T, N>,
        element: Option<T>,
    }

    // TODO: Is this safe?
    impl<'a, T, N: ArrayLength<T>> Unpin for ChannelSend<'a, T, N> {}

    impl<'a, T, N: ArrayLength<T>> Future for ChannelSend<'a, T, N> {
        type Output = ();

        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            self.inner.poll_enqueue(cx, &mut self.element)
        }
    }

    pub struct ChannelReceive<'a, T, N: ArrayLength<T>> {
        inner: &'a ChannelInner<'a, T, N>,
    }

    impl<'a, T, N: ArrayLength<T>> Future for ChannelReceive<'a, T, N> {
        type Output = T;

        fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            self.inner.poll_dequeue(cx)
        }
    }
}

/*
mod macros {
    #[macro_export]
    macro_rules! bind {
        ($device:expr, $name:ident, $ty:ty, $instance:expr) => {{
            mod $name {
                use drogue_device_platform_std::{Actor, ActorState, Forever};
                pub static DROGUE_ACTOR: Forever<ActorState<'static, $ty>> = Forever::new();

                #[embassy::task]
                pub async fn trampoline(state: &'static ActorState<'static, $ty>) {
                    let channel = &state.channel;
                    let mut actor = unsafe { (&mut *state.actor.get()) }; // state.actor.borrow_mut();
                    loop {
                        let mut pinned = core::pin::Pin::new(&mut *actor);
                        let request = channel.receive().await;
                        <$ty>::process(pinned, request).await;
                    }
                }
            }
            let a = $name::DROGUE_ACTOR.put(ActorState::new($instance));
            let addr = a.mount();
            $device.start($name::trampoline(a));
            addr
        }};
    }
}
*/
