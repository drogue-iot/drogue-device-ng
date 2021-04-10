use crate::channel::{consts, Channel};
use crate::signal::{SignalFuture, SignalSlot};
use core::cell::UnsafeCell;
use core::future::Future;
use core::pin::Pin;

pub trait Actor {
    type Message;
    type OnStartFuture<'a>: Future<Output = ()>
    where
        Self: 'a;
    type OnMessageFuture<'a>: Future<Output = ()>
    where
        Self: 'a;

    fn on_start(self: Pin<&'_ mut Self>) -> Self::OnStartFuture<'_>;
    fn on_message<'m>(
        self: Pin<&'m mut Self>,
        message: &'m mut Self::Message,
    ) -> Self::OnMessageFuture<'m>;
}

pub struct Address<'a, A: Actor> {
    state: &'a ActorState<'a, A>,
}

impl<'a, A: Actor> Address<'a, A> {
    pub fn new(state: &'a ActorState<'a, A>) -> Self {
        Self { state }
    }
}

impl<'a, A: Actor> Address<'a, A> {
    pub async fn send<'m>(&self, message: &'m mut A::Message) {
        self.state.send(message).await
    }
}

impl<'a, A: Actor> Copy for Address<'a, A> {}

impl<'a, A: Actor> Clone for Address<'a, A> {
    fn clone(&self) -> Self {
        Self { state: self.state }
    }
}

pub struct ActorState<'a, A: Actor> {
    pub actor: UnsafeCell<A>,
    pub channel: Channel<'a, ActorMessage<A>, consts::U4>,
    signals: UnsafeCell<[SignalSlot; 4]>,
}

impl<'a, A: Actor> ActorState<'a, A> {
    pub fn new(actor: A) -> Self {
        let channel: Channel<'a, ActorMessage<A>, consts::U4> = Channel::new();
        Self {
            actor: UnsafeCell::new(actor),
            channel,
            signals: UnsafeCell::new(Default::default()),
        }
    }

    fn acquire_signal(&self) -> &SignalSlot {
        let signals = unsafe { &mut *self.signals.get() };
        let mut i = 0;
        while i < signals.len() {
            if signals[i].acquire() {
                return &signals[i];
            }
            i += 1;
        }
        panic!("not enough signals!");
    }

    async fn send<'m>(&'a self, message: &'m mut A::Message) {
        let signal = self.acquire_signal();
        let message = ActorMessage::new(message, signal);
        self.channel.send(message).await;
        SignalFuture::new(signal).await
    }

    pub fn mount(&'a self) -> Address<'a, A> {
        self.channel.initialize();
        Address::new(self)
    }

    pub fn address(&'a self) -> Address<'a, A> {
        Address::new(self)
    }
}

pub struct ActorMessage<A: Actor> {
    message: *mut A::Message,
    signal: *const SignalSlot,
}

impl<A: Actor> ActorMessage<A> {
    fn new(message: *mut A::Message, signal: *const SignalSlot) -> Self {
        Self { message, signal }
    }

    pub fn message(&mut self) -> &mut A::Message {
        unsafe { &mut *self.message }
    }

    pub fn done(&mut self) {
        unsafe { &*self.signal }.signal();
    }
}

impl<A: Actor> Drop for ActorMessage<A> {
    fn drop(&mut self) {
        self.done();
    }
}
